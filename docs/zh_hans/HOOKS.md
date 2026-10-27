# 钩子（Hooks）

> 英文原文：[HOOKS.md](../HOOKS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

Hooks 会在 Codewhale **TUI** 到达生命周期节点时运行一条 shell 命令。它们是普通进程：通过环境变量接收上下文，其中一些会在 stdin 上收到 JSON 载荷，还有三个可以引导 Codewhale 接下来做什么。

本页是当前已实现内容的权威参考。与 `config.toml` 其余部分重叠的配置语法见 [CONFIGURATION.md](CONFIGURATION.md)；本文件是逐事件的约定说明。

## 适用范围

Hooks 会在交互式 TUI 和引擎回合循环中触发；Runtime API 也把这个回合循环放在桌面应用和 web 的背后驱动。

| 界面 | 是否触发 hooks |
| --- | --- |
| `codewhale` / `codew` 交互式 TUI | 是 |
| `codewhale exec`（无头一次性执行） | 需主动开启：`--hooks` 会触发 `tool_call_before` 和 `shell_env` |
| Runtime API 线程（桌面应用、web） | 是：`tool_call_before`、`shell_env`、`tool_call_after`、`on_error`；`GET /v1/hooks` 列出这一组 |
| `codewhale` CLI 分发器及其子命令 | 否 |
| app-server / ACP | 否 |
| `workflow` 工具和子智能体 *内部机制* | 否——但 TUI 会在它们周围触发 `subagent_spawn` / `subagent_complete` |
| 公共 API | 不存在 |

本仓库中的 `crates/hooks` event-sink crate 是一个无关的内部机制。它与这里描述的 hooks 不共享任何配置、事件名称或契约。

### `codewhale exec --hooks`

无头运行默认不触发任何 hook——CI 任务不应仅仅因为存在某份配置，就开始呼叫值班轮换。`codewhale exec --hooks` 让这次运行主动开启 hooks。引擎侧的事件是 `tool_call_before`（退出码 2 仍会拒绝该调用；`ask` 按失败即关闭（fail-closed）处理，因为无头模式下没有任何东西可以弹出提示）和 `shell_env`。由 UI 驱动的事件，例如 `session_start`、`message_submit` 和 `turn_end`，不会触发——它们属于交互式外壳，而不是回合循环。Fleet worker 子进程从不触发操作员 hooks。与这个标志无关，`permissions.toml` 中有类型的规则本来就适用于 `exec`——这次运行驱动的是同一个回合循环，而 `deny` 在每种模式下都会拦截。

## 快速开始

```toml
# ~/.codewhale/config.toml
[hooks]
enabled = true

[[hooks.hooks]]
name = "announce"
event = "session_start"
command = "echo 'Codewhale session started'"
```

在 TUI 中运行 `/hooks` 可以列出已配置的内容、全局开关是否开启，以及任何在加载时被拒绝的条目。运行 `/hooks events` 可查看事件名称。

## 配置

```toml
[hooks]
enabled = true                 # 全局开关；false 会抑制所有 hook
default_timeout_secs = 30      # 见下面的超时说明
working_dir = "/path/to/dir"   # 默认：会话工作区

[[hooks.hooks]]
event = "tool_call_before"     # 必填；下面 15 个名称之一
command = "~/.codewhale/hooks/gate.sh"  # 必填；Unix 上是 `sh -c`，Windows 上是 `cmd /C`
name = "gate"                  # 可选；/hooks 和日志行中的标签
timeout_secs = 30              # 可选，默认 30
background = false             # 可选；在 hook worker 内前台运行
continue_on_error = true       # 可选，默认 true
condition = { type = "tool_name", name = "bash" }  # 可选
```

`timeout_secs` 说明（按实现陈述）：当设置了 `[hooks].default_timeout_secs` 时，它会**覆盖**每个 hook 自己的 `timeout_secs`，而不仅仅是给省略该项的 hook 提供默认值。如果你希望各 hook 各自的超时生效，请保持不设置它。`/hooks list` 会显示运行时实际应用的超时，并在有覆盖生效时指明该覆盖。

`default_timeout_secs = 0` 会在**加载时被拒绝**。由于该值会替换每个 hook 自己的 `timeout_secs`，这里的零会让配置中的每个 hook 立即超时——包括 `tool_call_before` 门，从而拒绝每个匹配的工具调用。该覆盖会被忽略，各 hook 自己的 `timeout_secs` 生效，hooks 本身仍然会加载，拒绝情况由 `/hooks list` 在 *configuration problems* 下列出。每个 hook 自己的 `timeout_secs = 0` 也会被拒绝，但那只会丢弃写出它的那一个 hook。

Hooks 以工作区（或 `working_dir`）作为当前目录运行。

### 超时

超时对**前台和后台 hooks 一视同仁**。超时发生时：

- hook 的整个进程组会被杀死——Unix 进程组、Windows Job Objects——因此会派生子进程的 hook 不会活过它的预算；
- 子进程随后被回收，所以通常不会留下脱离或僵死的进程；
- 前台 hook 的结果为 `success = false`、`exit_code = None`、空的 `stdout`/`stderr`，以及 `error = "Hook timed out after Ns"`；
- 后台 hook 的超时会在 `hooks` 目标下以 `warn` 级别记录日志。不会向调用方报告任何内容，因为调用方在提交 hook 的那一刻就已停止等待。

**终止是尽力而为的，且被保证的边界是 Codewhale 的，而非操作系统的。** kill 可能无法落地——Unix 上进程卡在不可中断状态，Windows 上受保护进程能扛过 `TerminateJobObject`——任何用户态程序都无法承诺更多。Codewhale 保证的是它停止等待：释放 containment handle（这会重新向 Unix 进程组发信号，并关闭随关闭即杀的 Windows Job Object），回收只有一个短暂的有界窗口。如果子进程仍无法确认已死，会以 `warn` 级别记录，前台结果也会如实说明——`error = "hook could not be reaped after its timeout"` 而不是更强的超时措辞。因此，超时的 hook 永远不会阻塞回合，但请把"已杀死"视为尽力而为，而非绝对保证。

### 后台 hooks

`background = true` 描述的是真实的调度，而不只是一个配置标志。后台 hook 是**提交后绝不等待**的：

- 它以非阻塞方式进入固定的 32 项 supervisor 队列，由两个持续运行的 worker 消费并应用上述超时；队列饱和或 supervisor 丢失是一次失败的提交，任何一次调用都不会创建自己独立的分离 supervisor 线程；
- 它收到与该事件前台形式相同的环境变量和相同的 stdin JSON 载荷——载荷契约不变，变的只是引导能力；
- 它的 stdout 和 stderr 会被丢弃（`Stdio::null()`），因此它永远无法返回判定；
- 运行时交给调用方的 `HookResult` 会被标记为后台提交，且不携带退出码。引导代码读取 `observed_exit_code()`，对后台 hook 而言它是 `None`，因此后台 hook 永远无法 allow、deny、ask 或改写任何内容。

`shell_env` 完全忽略 `background`——它的 stdout *就是*契约，所以它总是前台运行。`/hooks list` 会将其报告为配置警告，并且不把该 hook 标注为 `[bg]`。

仅观察的 UI 事件通过非阻塞 `try_send` 提交到一个 32 项队列，由两个持续运行的 worker 消费。已配置的前台观察者仍会在某个 worker 内按配置顺序被等待，但终端事件循环从不等待它的进程，也从不按事件创建线程。队列饱和或分发器丢失会丢弃该观察者事件，并产生一个事件专属的错误 toast，不会被智能体普通的进度状态更新覆盖。引导事件保留其门或变换语义：fresh/queued `message_submit` 分发通过有界结果通道报告，同回合的引导在调用引擎引导路径之前于阻塞 worker 上执行变换，而 `tool_call_before` / `shell_env` 在引擎或工具 worker 上执行，而非终端事件循环。

### hook 进程环境

hook 命令继承 Codewhale 进程的环境，外加该事件对应的 `DEEPSEEK_*` 变量。Codewhale 不会过滤这种继承，所以请像对待你在启动 Codewhale 的同一个 shell 中键入的任何命令那样对待 hook：那里导出的任何内容对它都可见。

`shell_env` hook 提供的命令则*不是*这样——参见 [`shell_env`](#shell_env) 中管辖**本地** `exec_shell` 的有界 allowlist，以及改配置为外部 sandbox 后端时会发生什么变化（后端拥有自己的基础环境，你的 `shell_env` 值会被传输给它）。

### 条件

| 条件 | 匹配 | 支持于 |
| --- | --- | --- |
| `{ type = "always" }` | 每次调用（省略时的默认值也是它） | 每个事件 |
| `{ type = "tool_name", name = "bash" }` | 精确工具名；支持 `*` 通配，例如 `mcp__*`。shell 工具的写法 `bash`、`Bash` 和 `exec_shell` 互为别名：条件中写其中任何一个，都会匹配这三者 | `tool_call_before`、`tool_call_after`、`shell_env`、`on_error` |
| `{ type = "tool_category", category = "shell" }` | 工具类别 | `tool_call_before`、`tool_call_after`、`shell_env`、`on_error` |
| `{ type = "mode", mode = "plan" }` | 上下文的模式字符串，不区分大小写 | 除 `shell_env` 外的每个事件 |
| `{ type = "exit_code", code = 1 }` | 工具实际报告的退出码 | `tool_call_after`、`on_error` |
| `{ type = "all", conditions = [...] }` | 每个嵌套条件 | 每个事件 |
| `{ type = "any", conditions = [...] }` | 至少一个嵌套条件 | 每个事件 |

以下三条规则避免条件给出误导性的匹配结果：

- **`exit_code` 需要真实的退出码。** 它只在事件确实观察到进程退出码时匹配——`tool_call_after`，或工具失败时的 `on_error`，两种情况都针对 `bash` 这类由进程支撑的工具。以非零码退出的命令同样会报告它的退出码，尽管 `bash` 会把它作为一次失败的调用返回。超时或被杀死的命令通常没有退出码；`DEEPSEEK_TOOL_STATUS` 会说明是哪一种。不报告退出码的工具永远不会匹配 `exit_code` 条件；默认值、零或成功标志都不能满足该条件。该值是 64 位整数，因此 `3221225477`（`0xC0000005`）这样的 Windows 崩溃码也可以匹配。
- **支持工具作用域的 `on_error` hooks。** `on_error` 会因传输和容量错误*以及*工具失败而触发；工具失败的触发会携带工具名、调用 id、结果和报告的退出码。因此，`on_error` 上的 `tool_name` / `tool_category` / `exit_code` 条件是有效的配置。背后没有工具的 `on_error` 触发只是不匹配这样的条件——它在分发时被跳过，而不是在加载时被拒绝。
- **不支持的条件会在加载时被拒绝。** 引用其事件永远不会携带的上下文的条件永远无法匹配，带有这种条件的 hook 会静默失效——最危险的情形，是操作员以为已生效的 `deny` 拦截门实际上根本没有起作用。Codewhale 会在加载时丢弃这些 hooks，在 `hooks` tracing 目标下记录原因，并在 `/hooks list` 中显示为 `rejected:`。`all` / `any` 内的嵌套谓词也会被检查。带 `timeout_secs = 0` 或空 `command` 的 hook 也会以同样的方式被拒绝。拒绝是**逐条**的：一个坏 hook 永远不会连累另一个，即使两者共享同一个 `name` 或都未命名。

### 项目本地 hooks

仓库可以附带 `<workspace>/.codewhale/hooks.toml`，使用相同的结构，但只有它的 `[[hooks]]` 条目会被合并——项目文件不能更改 `enabled`、`default_timeout_secs` 或 `working_dir`，这些始终来自你自己的配置。由于 hooks 是可执行配置，项目 hooks **只有**在工作区受信任、并且在用户自有配置中对该 hooks 文件的确切内容另行批准之后才会加载。用 `/hooks review` 检查其中的命令和摘要（digest），再用 `/hooks approve <digest>` 让这些字节在下一个会话生效。命令所调用的脚本也请一并审阅。文件一旦改动，就需要重新批准。`/hooks revoke` 会阻止之后以及已排队的启动；它不会停止已经在运行的命令。仅靠会话内的 `/trust on` 不会启用项目 hooks。已批准的项目 hooks 会追加在全局 hooks 之后，因此它们最后运行，并在 `updatedInput` 平局时胜出。格式错误的受信任项目文件会记录一条警告，Codewhale 只回退到全局 hooks。校验针对合并后的集合运行，因此被拒绝的项目 hook 与被拒绝的全局 hook 报告方式相同。

## 15 个事件

| 事件 | 触发时机 | 引导 |
| --- | --- | --- |
| `session_start` | 一次，引擎就绪后、首次绘制前 | observer |
| `session_end` | 一次，优雅关闭时 | observer |
| `turn_end` | 回合完成且回合后状态更新后 | observer |
| `message_submit` | 在提交的消息到达历史或模型之前 | **可以替换或阻止文本** |
| `tool_call_before` | 每次工具调用执行之前 | **可以 allow / deny / ask、改写输入、添加上下文** |
| `tool_call_after` | 每个工具结果落定后，包括 transcript 不重绘的完成 | observer |
| `mode_change` | 每次应用的 Plan/Work/Operate 转换（`Act` 是 Work 的兼容别名） | observer |
| `on_error` | 传输、容量和认证错误，以及工具失败时 | observer |
| `subagent_spawn` | 子智能体启动时 | observer |
| `subagent_complete` | 子智能体完成、失败或被取消时 | observer |
| `shell_env` | 每次 `exec_shell` 调用之前 | **贡献环境变量** |
| `session_idle` | 会话在一个回合或一次等待之后回到空闲——没有未决的提示、审批或续跑 | observer |
| `session_error` | 一个回合以终止性失败结束时；智能体自行消化的瞬时工具失败不会触发它 | observer |
| `waiting_for_user` | 智能体开始等你时：审批提示打开、呈现了一个 `request_user_input` 问题，或者目标续跑在两次执行之间被挂起 | observer |
| `session_busy` | 空闲或等待中的会话开始或恢复工作时；启动时以及对同一状态的重复观察都保持静默 | observer |

`waiting_for_user` 的载荷带有 `reason`：`approval`、`user_input` 或 `goal_continuation`。三个状态事件都带有 `from`/`to` 转换字段；`session_idle` 在已知时还带有 `last_turn_status`，`session_error` 带有有界的终止性 `error` 文本。busy、idle 和 waiting 对应控制套接字的 `status` 动词已经发布的会话状态（`idle` / `in_progress` / `waiting`），因此 hook 与 supervisor 永远不会对会话正在做什么产生分歧。想要 opencode 那种针对错误告警的宽限期语义的 hook 作者，应当在 hook 内部去抖动——`session_error` 已经排除了被消化的瞬时失败，而一个失败后被操作员重试的回合，只有在重试同样以失败结束时才会再次触发。

### “observer”到底意味着什么

Observer 意味着 Codewhale 会忽略 hook 的**结果**：stdout 被丢弃，非零退出被记录为警告，回合、工具结果、子智能体或错误都不会因它而改变。

Observer 并**不**意味着无副作用。observer hook 是以你的凭据运行的任意 shell 命令。它可以写文件、推送提交、呼叫值班轮换，或删除工作区。它唯一做不到的是改变 Codewhale 自己接下来要做的事。

引导 allowlist 恰好是三个事件——`message_submit`、`tool_call_before`、`shell_env`——并且由一个覆盖每个变体的测试断言，因此新事件默认是 observer。

### 会话身份

同一个 TUI 会话中的每个事件携带相同的 `DEEPSEEK_SESSION_ID`。该 id 在启动时生成一次，形式为 `sess_xxxxxxxx`，并且能挺过工作区切换和添加项目 hooks 的信任决策——两者都会重新加载 hook 集，而不会开始新会话。引擎触发的 `tool_call_before` 与 UI 触发的事件报告相同的 id，因此工具记录可以与周围的会话记录关联。

`session_end` 在排队的启动默认写入被排空后、应用仍然存活时触发，因此它观察到的是落定的结束状态，而不是半拆除的状态。

## 环境变量

每个 hook 都会收到这些变量中适用于其事件的那一部分。`DEEPSEEK_` 前缀为兼容改版前编写的 hooks 而保留。

| 变量 | 设置于 | 说明 |
| --- | --- | --- |
| `DEEPSEEK_SESSION_ID` | 除 `shell_env` 外的每个事件 | `sess_xxxxxxxx`，整个会话保持稳定 |
| `DEEPSEEK_WORKSPACE` | 除 `shell_env` 外的每个事件 | 工作区绝对路径 |
| `DEEPSEEK_MODEL` | 除 `shell_env` 外的每个事件 | 当前生效的模型 id |
| `DEEPSEEK_MODE` | 除 `shell_env` 外的每个事件 | 见下面的模式拼写说明 |
| `DEEPSEEK_TOTAL_TOKENS` | UI 触发的事件 | 触发时的会话 token 总量 |
| `DEEPSEEK_MESSAGE` | `message_submit`、`subagent_*` | 截断至 5 000 字节并带 `...[truncated]` 标记 |
| `DEEPSEEK_ERROR` | `on_error` | 错误消息，截断至 5 000 字节 |
| `DEEPSEEK_PREVIOUS_MODE` | `mode_change` | 变更前的模式标签 |
| `DEEPSEEK_TOOL_NAME` | `tool_call_before`、`tool_call_after`、`shell_env`、`on_error`（工具失败） | |
| `DEEPSEEK_TOOL_CALL_ID` | `tool_call_before`、`tool_call_after`、`on_error`（工具失败） | 引擎调用 id；关联一次调用的 before/after/error |
| `DEEPSEEK_TOOL_ARGS` | `tool_call_before`、`shell_env` | 工具输入 JSON 预览，上限 10 000 字节 |
| `DEEPSEEK_TOOL_RESULT` | `tool_call_after`、`on_error`（工具失败） | 截断至 10 000 字节 |
| `DEEPSEEK_TOOL_SUCCESS` | `tool_call_after`、`on_error`（工具失败） | `true` / `false` |
| `DEEPSEEK_TOOL_EXIT_CODE` | `tool_call_after` 和 `on_error` **当工具报告了退出码时** | 否则不存在——绝不合成；命令失败时同样设置；64 位，因此 `3221225477` 这样的 Windows 崩溃码能完好保留 |
| `DEEPSEEK_TOOL_STATUS` | `tool_call_after` 和 `on_error` **当 shell 工具报告了状态时** | `completed`、`failed`、`timed_out`、`killed` 或 `running`（已转入后台）；其他工具不存在 |
| `DEEPSEEK_TOOL_EXECUTION_RECEIPT` | `tool_call_after` 和 `on_error` **仅限已结束的本地前台 shell 运行** | 完整 JSON，最多 32 KiB，否则不存在；见下方“执行回执” |
| `DEEPSEEK_SESSION_COST` | 提供成本时 | USD，六位小数 |

### 执行回执

`DEEPSEEK_TOOL_EXECUTION_RECEIPT` 说明 shell 工具（`bash`、`Bash`、`exec_shell`）实际运行了什么。before-hook 的输入不等于实际执行的内容：`tool_call_before` hook 可以改写它。回执取自进程管理器在准入与改写之后启动进程时记录的内容。

```json
{"schema_version":1,"command":"printf hello","cwd":"/absolute/workspace","state":"completed","scope":"local","exit_code":0,"stdout":"hello","stderr":"","stdout_truncated":false,"stderr_truncated":false,"output_kind":"separate"}
```

| 字段 | 含义 |
| --- | --- |
| `command` | 交给 shell 的已准入命令源码，而不是 shell 可执行文件或其 argv 包装 |
| `cwd` | 进程启动时所在目录的规范绝对路径：解析符号链接，因此无论调用是否传入 `cwd`，同一目录只有一种写法；在启动前解析，并将同一路径交给操作系统 |
| `state` | 观察到退出（包括非零退出）为 `completed`；信号、kill、取消或超时为 `interrupted` |
| `scope` | schema 1 中始终为 `local` |
| `exit_code` | 观察到的整数，或 `null`；绝不根据 `state` 合成 |
| `stdout`、`stderr` | 工具已保留输出的预览，其中可能已经缺少进程输出的开头；过长的预览保留自身的首尾字节，中间以 `[receipt preview truncated]` 标记 |
| `stdout_truncated`、`stderr_truncated` | 工具自身的输出捕获或预览丢弃了字节时为 `true` |
| `output_kind` | `Bash` / `exec_shell` 为 `separate`；小写 `bash` 的 stdout 与 stderr 共用一个管道，为 `combined`——此时 `stdout` 是合并后的预览，`stderr` 为空 |

规则是保守的：

- **要么精确，要么不存在。** `command` 和 `cwd` 绝不截断。任一超过 8 KiB、包含 NUL，或目录是相对路径、非 UTF-8 或在启动前无法解析时，不导出回执。shell 工具无法观察到运行如何结束（操作系统的 wait 调用本身失败）时同样不导出：其状态未知，回执不做猜测。预览会缩短直到序列化后的 JSON 不超过 32 KiB；仍然放不下时不导出回执，而不是截断。
- **不存在不代表任何结果。** 既不意味着成功，也不意味着失败。应用当前调用的上下文前，会清除继承的 `DEEPSEEK_TOOL_EXECUTION_RECEIPT`。
- **范围。** 只有配置了 `tool_call_after` 或 `on_error` hook 时才会生成回执，且仅限已结束、基于管道、未沙箱化的本地前台运行。后台启动、转入 `/jobs` 的前台运行、PTY（`tty` / `combined_output`）与交互会话、OS 沙箱与外部后端执行、只读 shell 的加固 argv、Windows、任何平台上的 PowerShell（它会包装源码或通过临时脚本运行），以及执行前就被拒绝的调用都没有回执。
- **仅供 hook 使用。** 回执不写入持久化的 Runtime API 条目记录；该记录已包含工具输出。
- 失败的运行与成功的运行同样设置，因此 shell 调用失败时的 `on_error` 也带有它。其他变量均不变。

对于 `tool_call_after`，同一份执行证据还会作为带版本号的 JSON 文档通过 stdin 传递，前台和后台 hook 均会收到：

```json
{"schema_version":1,"event":"tool_call_after","tool_name":"bash","session_id":"session-id","tool_call_id":"call-id","session_id_truncated":false,"tool_call_id_truncated":false,"tool_name_truncated":false,"execution_receipt":{"schema_version":1,"command":"printf hello","cwd":"/absolute/workspace","command_truncated":false,"cwd_truncated":false,"execution":"started","completion":"completed","exit_code":0,"stdout":"hello","stderr":"","stdout_truncated":false,"stderr_truncated":false,"output_mode":"combined"}}
```

stdin 载荷不会改变上述环境变量中的回执。`completion` 表示观察到的终态：`completed`、`failed`、`killed` 或 `timed_out`；非零退出对应 `failed`。`exit_code` 保持为有符号 64 位整数或 `null`。`output_mode` 沿用现有回执的 `output_kind`，值为 `separate` 或 `combined`。输出合并时，空的 `stderr` 并不表示命令没有向 stderr 写入内容。

完整文档的上限为 64 KiB。关联标识符各自最多保留 1,024 个 UTF-8 字节，另加截断标记，并携带各自的截断标志；缺失的标识符为 `null`。shell 名称保持精确。执行的 `command` 和 `cwd` 仍遵守上述“要么精确，要么不存在”的规则，因此它们的截断标志始终为 `false`。输出截断标志同时反映捕获阶段与预览阶段丢弃的内容。

只有带有有效且已落定回执的原生 shell 调用才会生成这份 stdin 文档。不支持或未观察到结果的路径不会生成文档；缺失仍表示未知。hook 仍然只观察结果：其 stdout 不能允许、拒绝或改写已完成的调用，后台 hook 也不会被等待。`on_error` 继续只接收环境变量中的回执。

**模式拼写说明。** UI 触发的事件（`session_start`、`session_end`、`message_submit`、`tool_call_after`、`mode_change`、`on_error`、`turn_end`、`subagent_*`、`session_busy`、`session_idle`、`session_error`、`waiting_for_user`）会将 `DEEPSEEK_MODE` 设为 UI 标签——`ACT`、`PLAN`、`OPERATE`。`tool_call_before` 在引擎内部触发，并使用引擎自己的模式拼写（`Agent`、`Plan`、`Operate`）。`mode` 条件不区分大小写比较，因此 `{ type = "mode", mode = "plan" }` 两者都能匹配，但精确字符串匹配 `$DEEPSEEK_MODE` 的 hook 应同时接受两种拼写。

**`shell_env` 是受限的那个。** 它只接收 `DEEPSEEK_TOOL_NAME` 和 `DEEPSEEK_TOOL_ARGS`——没有会话 id、工作区、模型或模式。因此，`shell_env` hook 上的 `{ type = "mode", … }` 条件会在加载时被拒绝；请改用 `tool_name` 或 `tool_category` 来限定作用域。

## 引导事件

### `message_submit`

在 stdin 上接收 JSON，并可能改写或阻止提交的文本。

```json
{
  "event": "message_submit",
  "text": "original user text",
  "text_bytes": 18,
  "text_original_bytes": 18,
  "text_truncated": false,
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "ACT",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

完整的序列化 stdin 文档上限为 32 KiB。`text` 是在包含 JSON 转义和有界元数据后能容纳的最大确定性 UTF-8 前缀。`text_original_bytes` 记录生产者的完整字节长度，`text_bytes` 记录保留的前缀，`text_truncated` 说明两者是否不同。同样的边界适用于即时输入、恢复的队列条目、合并的引导，以及先前 hook 产生的文本。

- 以退出码 `0` 打印带非空字符串的 `{"text": "..."}` 会替换文本
- 退出码 `0` 但 stdout 为空，或 JSON 中没有 `text`，文本保持不变
- `{"text": ""}` 或超过 32 000 字符的替换是无效 stdout，会被记录并忽略
- 退出码 `2` 会在进入历史或分发之前阻止提交；结构化的 `reason` 字段提供一条有界、脱敏的消息显示在 TUI 中。非结构化的 stdout/stderr/error 输出绝不会被复制进拒绝信息
- 其他非零退出遵循 `continue_on_error`：`true` 警告并继续，`false` 阻止提交
- `background = true` 使 hook 仅观察——它仍然会在 stdin 上收到这个有界载荷，但无法变换或阻止

多个 `message_submit` hooks 按配置顺序运行，每个都会看到前一个 hook 的输出。

### `tool_call_before`

通过环境变量接收工具上下文，并可以退出码 `0` 在 stdout 上打印 JSON 判定：

```json
{
  "decision": "allow",
  "reason": "human-readable explanation, used for deny",
  "updatedInput": { "command": "ls -la" },
  "additionalContext": "text appended to the tool result for the model"
}
```

- `deny` 阻止该工具；模型会收到携带 `reason` 的权限拒绝结果
- `ask` 在 Ask 和 Auto-Review 中强制交互式审批提示。Full Access 不会打开工具审批提示，因此 `ask` 不会降级它
- `updatedInput` 必须是序列化后不超过 32 KiB 的对象，并替换工具输入；最后一个 hook 胜出
- `additionalContext` 以 `[hook context] ...` 追加到工具结果；多个 hooks 会拼接
- `reason` 和 `additionalContext` 在使用前有界并净化：每个字段上限 2 000 字符，一次工具调用拼接后的上下文上限 8 000，控制字符会被剥离（因此 hook stdout 无法重绘 TUI 或在 transcript 中伪造结构），被截断的值携带 `…[truncated]` 标记。因此，无论 hook 打印什么，它为回合上下文预算贡献的内容都是有界的
- 退出码 `2` 是遗留的硬拒绝，无论 stdout 是什么都胜出
- 空 stdout、非 JSON stdout 以及没有 `decision` 的 JSON 都意味着 allow
- 匹配 hooks 之间的优先级：无判定且 `continue_on_error = false` > deny > ask > allow
- `background = true` 的 hooks 会被提交且从不等待，因此它们没有判定，也无法引导；Codewhale 在为此事件配置了这样的 hook 时会记录一条警告

**无法作答的门不是许可。** 如果前台 `tool_call_before` hook 没有产生判定——它超时了、进程无法启动，或严格进程在没有显式 JSON 判定的情况下以非零退出——并且*那个 hook* 配置了 `continue_on_error = false`，则该工具调用会被拒绝。严格性从实际运行的 hook 读取，而非从事件读取：条件未匹配 `exec_shell` 调用的严格 `write_file` 门，对该调用是否继续没有发言权；宽容 hook 的超时也绝不会仅仅因为配置中存在其他严格 hook 就拒绝。无论哪种情况，每个无判定结果都会被记录。

拒绝消息只指名 hook 和原因，别无其他：hook 名被截断，细节被截断，控制字符被剥离，spawn 失败按错误种类（`NotFound`、`PermissionDenied`、…）报告，而不是回显命令行或解析后的解释器路径。

### `shell_env`

在每次 `exec_shell` 之前同步运行，其 stdout 被解析为 `KEY=VALUE` 行。开头的 `export ` 会被剥离，`#` 注释行和空行会被跳过，值周围成对的单引号或双引号会被移除。后运行的 hooks 覆盖先运行的。用它来处理临时凭据、按 skill 调整 `PATH`，或短命 token。

`background` 对此事件被忽略：hook 总是前台运行，因为它的 stdout 就是契约。

shell 无法承载的条目会被丢弃，而不是放任其破坏工具调用：空名称；含空白、`=`、控制字符或 NUL 的名称；含 NUL 的值；超过 32 KiB 的值；以及单个 hook 累计输出超过 256 KiB 的任何内容。每次丢弃只按键名记录日志。`shell_env` hook 是普通进程，其 stdout 可以包含任何内容——"hook 打印了奇怪的东西"绝不能变成"`exec_shell` 调用中止了"。

**shell 命令最终确切得到什么——本地执行。** 当 `exec_shell` 在本地运行命令（默认情况）时，它不继承 Codewhale 的环境。它的环境按如下方式构建：

1. 一份净化的固定父变量 allowlist——`PATH`、`HOME`、`USER`、`LANG` 和其他 `LC_*`/locale 条目、`TERM`、`SHELL`、`TMPDIR`、`proxy` 相关变量、`NO_COLOR` 这类颜色/终端条目、`CARGO_HOME`/`RUSTUP_HOME`/`RUSTUP_TOOLCHAIN`、Windows 系统与 MSVC 工具链条目，以及其他平台相关的键（完整列表在 `crates/runtime/src/child_env.rs`）——仅此而已。allowlist 之外的变量，包括任何看起来像密钥的内容，都会被丢弃；
2. 然后，你的 `shell_env` hooks 产生的 `KEY=VALUE` 对叠加应用在上面。这些是你配置的显式值，因此它们胜过 allowlist。

因此，`shell_env` hook 是把凭据送进一次本地 `exec_shell` 调用的受支持方式。启动 Codewhale 的终端中导出的环境变量里的密钥**不会**自行转发给本地 `exec_shell`。

**配置了外部 sandbox 后端时，上面的 allowlist 不是契约。** 如果 `exec_shell` 被路由到已配置的 sandbox/执行后端，Codewhale 根本不会构建进程环境：它把命令和你的 `shell_env` 值作为额外环境变量交给后端，**后端拥有自己的基础环境**。除了你的值之外还存在什么——镜像内置的变量、后端自己的注入、远程 runner 导出的任何内容——由该后端决定，而非由上面的列表决定。不要假定本地 allowlist 在那里适用。

披露说明，因为这对发出凭据的 hook 才是关键部分：**`shell_env` 值会被传输到已配置的后端。** 对远程或容器化后端而言，这意味着这些值会离开本机，并受该后端的日志记录、保留和访问控制约束。Codewhale 自己的审计日志仍然只记录键名，但这并不能说明后端会对这些值做什么。如果 `shell_env` hook 会输出密钥，请将其限定在你信任、可以托付该密钥的后端上——例如给 hook 加条件，或在这些 hooks 生效的会话中不配置外部后端。

解析出的**键名——绝不是值**——会写入 `~/.codewhale/audit.log`，以便事后对会话进行核对。失败或超时的 hook 不贡献任何变量，也不会中止 shell 调用。

```toml
[[hooks.hooks]]
name = "aws-creds"
event = "shell_env"
command = "aws-vault export my-profile --format=env"
condition = { type = "tool_category", category = "shell" }
```

## 结构化 observer 载荷

`turn_end`、`subagent_spawn`、`subagent_complete`、`session_busy`、`session_idle`、`session_error` 和 `waiting_for_user` 除了环境变量外，还会在 stdin 上接收 JSON。它们的 stdout 被忽略。这些事件的后台形式会在 stdin 上收到相同的载荷。

`tool_call_after` 在原生 shell 调用结束且具有已记录的[执行回执](#执行回执)时，也会通过 stdin 接收 JSON，前台和后台形式均如此。其他工具调用没有 stdin 文档。

其余 observer 事件——`session_start`、`session_end`、`mode_change`、`on_error`——无论前台还是后台形式，都只接收环境变量，没有 stdin 载荷。

### 会话状态转换

第一个观察到的状态会被静默记录，无论是 idle、busy 还是 waiting。重复同一状态不会发出任何事件。对于一个中途暂停等待用户输入、然后完成的回合，转换类 hooks 会按提交顺序收到这些载荷：

| 事件 | stdin JSON |
| --- | --- |
| `session_busy` | `{"from":"idle","to":"in_progress"}` |
| `waiting_for_user` | `{"from":"in_progress","to":"waiting","reason":"user_input"}` |
| `session_busy` | `{"from":"waiting","to":"in_progress"}` |
| `session_idle` | `{"from":"in_progress","to":"idle","last_turn_status":"completed"}` |

分发器有两个 worker，因此不保证命令的完成顺序。`session_error` 是一个独立的终止性失败事件，带的是 `status` 和 `error` 字段，而不是 `from` 和 `to`。

### `turn_end`

在回合后状态、用量总计、成本核算、通知、回执和队列恢复都已更新之后、排队的后续分发之前触发——这样载荷可以报告排队数量，而 hook 无法改变接下来要发送的内容。

```json
{
  "event": "turn_end",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "ACT",
  "created_at": "2026-07-12T10:30:00+00:00",
  "model_backed": true,
  "provider": "deepseek",
  "billing_surface": null,
  "model": "deepseek-chat",
  "turn_id": "turn_12345678",
  "status": "completed",
  "error": null,
  "duration_ms": 1834,
  "usage": {
    "input_tokens": 1200,
    "output_tokens": 180,
    "prompt_cache_hit_tokens": 900,
    "prompt_cache_miss_tokens": 300,
    "prompt_cache_write_tokens": 0,
    "reasoning_tokens": null,
    "reasoning_replay_tokens": null
  },
  "totals": {
    "session_tokens": 1380,
    "conversation_tokens": 1380,
    "input_tokens": 1200,
    "output_tokens": 180
  },
  "tool_count": 2,
  "queued_message_count": 1,
  "stop_hook_active": false
}
```

`created_at` 锚定时间窗口定价。`provider` 和 `model` 标识模型支撑回合的有效路由。`billing_surface` 是对服务该回合的端点的一种可选、不含敏感信息的分类（已识别的 StepFun 路由会发出 `stepfun-payg` 或 `stepfun-plan`）；原始 base URL 永远不会写入 hook 记录。仅 shell、手动压缩和 purge 完成没有对应的 `TurnStarted`，因此它们报告 `model_backed: false`、`null` provider 和合成的 `lifecycle_<uuid>` 回合 id。`stop_hook_active` 目前始终为 `false`；它为防重入保护预留了空间。

### `subagent_spawn` / `subagent_complete`

```json
{
  "event": "subagent_complete",
  "agent_id": "agent_1",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "ACT",
  "model": "deepseek-chat",
  "total_tokens": 1234,
  "result_preview": "bounded preview of the result",
  "result_truncated": false,
  "status": "completed"
}
```

`subagent_spawn` 改为携带 `prompt_preview` / `prompt_truncated`，且没有 `status`。两个载荷都有意设了界：预览被截断，而不是传送完整提示或结果。这些 hooks 仅观察——失败不会影响子智能体调度、提示或结果，`continue_on_error` 没有效果，因为后面匹配的 hooks 总是会运行。

## 失败行为

- 非零退出会在 `hooks` tracing 目标下以 `warn` 级别记录日志，包含 hook 名、事件、退出码、时长和一个通用失败类别。原始 stdout/stderr/error 文本不会持久化在日志回执中。
- 对于 `execute` 路径的事件，`continue_on_error = false` 会停止该事件后续的 hooks；除 `tool_call_before`（见上文）外，它不会回滚触发它们的行为。
- 结构化 observer 事件（`turn_end`、`subagent_*`、`session_busy`、`session_idle`、`session_error`、`waiting_for_user`）总是继续到下一个匹配的 hook。
- Observer 事件使用有界的持久分发器。队列已满和分发器不可用的提交不会静默重试；TUI 会保留一条事件专属的错误 toast，与普通状态行分开。
- 超过超时的 hook，其整个进程组会被杀死，然后被回收，前台或后台皆然——尽力而为，回收等待有界；见[超时](#超时)。

## 安全说明

- Hooks 是来自你自己配置的任意 shell 命令；请把 `~/.codewhale/config.toml` 当作可执行文件对待。
- 项目提供的 hooks，除了工作区信任之外，还需要在用户自有配置中对确切文件作出批准。
- hook 命令继承 Codewhale 自己的环境。本地 `exec_shell` 不会——见 [`shell_env`](#shell_env)。
- `shell_env` 审计记录只包含键名。这覆盖 Codewhale 自己的日志记录；配置了外部 sandbox 后端时，值本身会被传输到该后端，之后受其处理方式约束。
- 使用外部 sandbox 后端时，本地父变量 allowlist 不适用——后端拥有自己的基础环境。
- 载荷预览、工具参数/结果、错误消息、捕获的 stdout 和 stderr、替换消息和引导对象都有界，因此 hook 输入或输出不可能成为 transcript 的无界副本。
- Codewhale 在拒绝信息中持久化的任何内容都不会回显 stdin 载荷、hook 环境、原始 stdout/stderr/error、命令行或解析后的文件系统路径。`/hooks list` 显示净化后的单行命令预览，上限 60 字符；它不是逐字副本。结构化拒绝原因有界，并对类似路径、参数、命令和密钥的 token 脱敏，包括带引号或 `key=value` 的形式以及 `Authorization: Bearer …`。
