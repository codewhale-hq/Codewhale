# 工具表面（tool surface）

> 英文原文：[TOOL_SURFACE.md](../TOOL_SURFACE.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。
> 2026-10-06 补齐本地解释器权限与后端限制；现有 MCP 有界发现段落已核对。

本文描述当前面向模型的工具（tool）契约。产生它的 v0.9.1 切换记录在
`docs/RUNTIME_SIMPLIFICATION_DESIGN.md` 中；工作区版本请从 `Cargo.toml` 读取，
不要从本文这一处读。注册表（registry）仍比首回合（turn）目录更大，
这样已保存的转录（transcript）可以重放，不常见的能力也能按需加载。
模型对每种常见操作应当只学一个规范名称。

实现来源：

- `crates/tui/src/core/engine/tool_catalog.rs` 掌管预加载（eager）与延迟（deferred）目录。
- `crates/tui/src/tools/registry.rs` 注册规范工具与隐藏别名。
- `crates/tui/src/tools/{file,file_tool,shell}.rs` 掌管小型前台原语的行为与 schema；
  其余原生工具仍可被搜索。
- `docs/RUNTIME_SIMPLIFICATION_DESIGN.md` 记录了 v0.9.1 切换及其回执（receipt）。

## 默认激活契约

新回合开始时带有十二个预加载（eager）的原生名称，外加合成的 `tool_search`：

1. `read`
2. `write`
3. `edit`
4. `file_search`
5. `bash`
6. `agent`
7. `workflow`
8. `todo_write`
9. `create_goal`
10. `get_goal`
11. `update_goal`
12. `load_skill`
13. `tool_search`（合成，始终激活）

这十二个原生名称就是 `crates/tui/src/core/engine/tool_catalog.rs` 里的
`DEFAULT_ACTIVE_NATIVE_TOOLS`，由
`default_active_contract_keeps_discovery_and_core_tools_eager` 固定住。
权限边界（authority boundary）可以在子智能体达到最大深度时移除 `agent`，
但仅凭路由大小不得改变这套核心词汇。

直接 schema 刻意保持精简：

| 工具 | 输入 | 用途 |
|---|---|---|
| `read` | `path`、可选的 `offset`、可选的 `limit` | 读取一个有界的文件窗口，并给出明确的续读或截断提示。 |
| `write` | `path`、`content` | 创建或替换文件。 |
| `edit` | `path`、`edits` | 针对同一份原始快照应用一处或多处无歧义的文本替换。 |
| `bash` | `command`、可选的 `timeout` | 运行一条可取消的前台 shell 命令，并返回有界的尾部输出。 |
| `agent` | 被委派的任务以及可选的作用域/上下文控制项 | 启动或查看专注的子智能体任务。 |
| `workflow` | plan/script/source_path 加上运行控制项 | 用依赖关系和完成检查来协调多智能体阶段。 |
| `todo_write` | `{content, status}` 条目的完整替换列表 | 为真正的多步工作保留可选的、由智能体自己维护的进度笔记。 |
| `create_goal` | 目标文本以及可选预算 | 启动本回合所追求的会话目标（goal）。 |
| `get_goal` | 无 | 读取当前生效的目标及其进度。 |
| `update_goal` | 终态状态 | 把目标标记为完成或受阻。 |
| `tool_search` | `query`、可选的匹配控制项 | 发现策略允许的延迟工具，并把选中的 schema 加入本次对话的工具箱。 |

模式是一项权威决定，而不是同义词体系。Plan、Work 和 Operate 使用同一套原语身份。
Plan 在中心位置拒绝 `write`、`edit` 和 `bash`；Work 与 Operate 仍会让这些调用通过
审批（approval）、沙箱（sandbox）、受信任路径、仓库规则（repository law）和托管策略等闸门。
Full Access（完全访问）会改变常规审批行为，但不会绕过硬性安全限制或仓库规则。

`update_plan` 仅为已保存工件（artifact）的兼容性而保留注册，对模型不可见。
`tasks`、`Git`、`Run`、`Web`、`remember` 以及其他专门能力都是可搜索的，
而不是首回合的必备仪式。

## 本地代码执行

`code_execution`（Python）与 `js_execution`（Node.js）使用工作区任务门禁/测试
共用的权限感知启动器。常规调用保留会话策略。对被拒绝的精确调用申请更宽权限时，
提供 `sandbox_permissions`（`workspace-write` 或 `danger-full-access`）与非空
`justification`；Ask 模式需用户明确批准一次。获准策略只作用于该调用。

两种工具从 stdin 读代码，返回 stdout / stderr / return_code，保留超时及进程树
清理。它们不是持久 REPL；异常堆栈指向 stdin。独立 RLM/REPL 内核不受此次启动器
整合影响。共享启动器的平台限制仍适用：没有可用的本地包装器时，workspace-write
无法强制执行，read-only 则被拒绝。外部 sandbox 会话必须使用自己的 shell 路径，
不能借此运行本地解释器。参见[沙箱限制](SANDBOX.md)。

## 延迟与动态工具

`Web` 是有条件的、延迟的。只有当生效的策略与运行时（runtime）后端允许时，
它才能通过 `tool_search` 被发现。只读子智能体仍保留其只读的搜索/抓取证据路径；
只读权限并不意味着“无法做研究”。

可持久使用的 `github`、`automation` 和 `rlm` 动作族默认也是延迟的。
`rlm` 掌管一个持久本地 Python 会话（一个清理过环境变量的子进程，而不是操作系统级沙箱）的
`open`、`eval`、`configure` 和 `close` 动作。回复中内联的 ```` ```repl ```` 围栏也在同类内核中运行，
但仅当 `code_execution` 出现在该回合的工具表面上（Plan 模式下永远不会）、围栏独占一行开头，
并且在会话审批姿态下通过了 `code_execution` 的审批之后。
受特性开关控制的原生工具，只有在实现与宿主依赖都可用时，
才可以加入激活或延迟目录。

MCP 工具是动态的。连接成功的服务器会从 `~/.codewhale/mcp.json` 注册
诸如 `mcp_<server>_<tool>` 这样的名称；失败或已禁用的服务器不得被呈现为可用。
除非用户在 `[tools].always_load` 中明确点名，MCP 与插件工具都是延迟的。

对于尚未启动的已配置服务器，面向 MCP 的 `tool_search` 先在当前回合的服务器与
工具权限范围内执行有界发现。连接后搜索服务器实际提供的模式，不会虚构
`mcp_*` 定义。在 `execute_tools` 内搜索只描述模式而不激活；直接搜索沿用现有的
有界激活缓存。无关的普通搜索不会启动可选服务器。CLI 的独立 MCP 检查命令
使用单独的连接池。

### 代码模式（`execute_tools`）

`execute_tools` 与合成的解释器工具一样由引擎注入。它运行一个 JavaScript 程序，
该程序唯一的宿主表面是 `await tools.call(name, args)`；它也是组合多次工具调用
——包括 MCP 与插件工具——的默认方式，无需让每个中间结果都在对话里往返一遍。
它在 Plan 模式下被隐藏，在 worker 的权限范围（authority envelope）内会被拒绝。

- **一道闸门。** 在会话（session）回合中，每个嵌套调用都会被送回回合循环，
  并像直接调用一样被规划：拒绝/允许清单、准备工作
  （MCP 的 `readOnlyHint`/`destructiveHint`）、`tool_call_before` 钩子（hook）、
  ask 规则、Auto-Review、仓库规则，以及 Computer Use 的同意拒绝。
  MCP 调用走会话 MCP 池。批准程序本身不会授予任何权限，所以
  `execute_tools` 自身是自动批准的。如果程序运行期间权限姿态（posture）发生变化，
  它余下的嵌套调用会被拒绝（已批准的调用只有在姿态相同或更宽时才能存活，
  与直接调用一样），模型会在新姿态下重试它们。
- **审批会挂起程序。** 需要审批的嵌套调用会弹出常规审批卡片
  （名称为 `execute_tools program call: ...`），程序随即等待；允许则恢复它，
  拒绝只会让那个嵌套调用失败，成为程序可以捕获的异常。
  等待决定所花的时间不计入程序的运行截止时间，后者是本回合剩余的墙钟时间。
- **回执。** 结果会列出每个嵌套调用及其决定（`auto`、`approved`、`denied`、
  `refused`）和状态（`ok`、`failed`、`refused`、`in_flight`）。
  达到截止时间的程序仍会返回回执；`in_flight` 的调用已被取消，可能已部分运行。
  每个嵌套结果是 `{content, metadata, truncated}`；过大的结果保持这一形状，
  `truncated` 会标出原始大小以及存放完整输出的溢写文件。
- **发现而不重新固定。** 在程序内部，
  `tools.call('tool_search', {query})` 会返回匹配的延迟工具及其输入 schema，
  并且不会激活它们，因此请求的工具数组和会话固定的前缀都不会改变。
- **保持直接：** `agent`、`workflow`、`request_user_input`、嵌套的
  `execute_tools`、交互式 shell、沙箱（sandbox）升级、Computer Use
  同意与脚本，以及 MCP 登录（`mcp_<server>_authenticate`）。

代码模式默认开启（`[features] code_mode = true`），这会让 `execute_tools`
从第一次请求起就是预加载的；直接工具和 `tool_search` 两种情况下都保持可用。
把 `code_mode = false`（或用 `--disable code_mode` 运行）设回去，
`execute_tools` 就又会被延迟到 `tool_search` 之后。该开关属于会话配置，
因此提示词（prompt）前缀在一个会话内保持稳定。在没有引擎回合时（子智能体），
程序保持保守配置：只读、仅自动批准的原生调用，不使用 MCP。

### 对话工具箱缓存

一次成功的搜索激活会按名称记入当前对话。缓存最多保存八个延迟名称和
16 KiB 的序列化 schema，按最近最少使用淘汰条目，并在再次对外告知之前，
让每个条目对照当前目录与策略重新校验。会话同步会清空它。
缓存无法让已移除、已被拒绝或刚刚变为预加载的工具复活。

每个子智能体都有自己的、经策略过滤的延迟目录，始终存在的 `tool_search`，
以及有界的激活缓存。分叉出来的消息和指令仍留在上下文（context）中，
但子智能体的缓存从空开始，并在本地发现工具；分叉的上下文和缓存都不能变成发现白名单。
子智能体仍能搜索其自身权限所允许的每一个工具，包括只读研究角色使用的 Web 搜索/抓取。

## 检查模型客户端请求里的工具载荷

在模型回合之后运行 `/tools`，检查最近一次准备好的模型客户端请求中那个确切的
工具字段的有界投影。`/tools json` 以有界的机器可读 JSON 输出同样的证据。
两种格式都在分页器中打开；它们不会被复制进转录历史。`/tool-studio`
仍是人类命令的兼容别名；它不是模型工具。

快照把“工具字段缺失”与“存在但为空数组”区分开来。只有当测量值落在
1 MiB 的检查上限之内时，它才会报告模型客户端工具 JSON 的确切字节数和
SHA-256 摘要；更大的载荷保持不可用。提供商（provider）适配器在构建
提供商专属的实际传输请求体时，可能会转换、净化或省略这些字段，因此 `/tools`
会把提供商投递和实际传输的载荷标记为不可用。捕获与渲染都是有界的：保留的 schema、
描述、调用方列表、目录行、回合 ID 和载荷测量，都带有明确的截断、省略或不可用回执。
快照只在当前会话期间留在内存里，并在每次准备好请求时被替换。

提供商、模型、审批、注册表来源和运行时能力元数据都不是请求工具 schema 里的字段。
因此 `/tools` 会把它们报告为不可用，而不是去关联可变状态或推断取值。
这些事实请使用单独的路由与权限回执。

## 模式与权限姿态

模式与权限姿态是彼此独立的控制项：

- **Plan** 保持稳定的原语词汇，但在中心位置拒绝 shell 执行和文件改动。
- **Work** 是常规的交互式执行。
- **Operate** 使用与 Work 相同的直接工具权威。小规模工作保持直接进行；
  多步委派使用一个紧凑的 Workflow 计划，带依赖关系、有界范围和完成证据。
  Fleet（智能体团队）管理同一批智能体和角色。一个独立的有界任务可以直接使用 `agent`；
  `followup` 会复用该智能体继续工作。
- **Ask**、**Auto-Review** 和 **Full Access** 控制在具备行动能力的模式内的审批行为。
  它们绝不会把 Plan 放宽为写入或 shell 访问。

完整的模式与姿态契约见 `docs/MODES.md`。

## 兼容名称

面向模型的契约是上面的小写核心。已保存的 v0.9.x 转录和协议客户端仍可能调用
确切的隐藏兼容名称，例如 `File`、`Bash`，以及更早的单操作文件名称。
这些名称绝不会进入新的模型目录或 `tool_search` 结果。

兼容是执行层面的兼容，而不是模糊别名：一次确切的旧版调用必须到达其旧版 schema
的处理函数。它不得被改写成输入形状不同的小写原语。未知或已退役的名称
仍然失败关闭（fail closed），而不是去猜一个目的地。

`Git`、`Run`、`Web` 这类专门的原生族不是小写核心的别名。它们仍是真实的、
经策略过滤的延迟工具，在需要时通过 `tool_search` 加载。

## 长时间运行的工作

`bash` 只运行一条可取消的前台命令。它不携带 background、TTY、wait、interact
或 cancel 动作字段。有状态的进程与终端控制属于专门功能，必须被显式发现；
它不会扩大首回合的 shell schema。

当工作本身需要可持久化的生命周期、结构化闸门、工件、可重放时间线或稳定的
任务 id 时，请使用 `tasks`。大型工具结果应当留在有界的句柄或工件之后，
而不是整份复制进父级转录。

## 并行扇出

智能体（子智能体）容量的唯一事实来源是 `crates/tui/src/config/subagent_limits.rs`：

- 默认配置并发数：**64**；
- 最大配置并发数：**128**；
- 允许的运行中加排队工作上限：**1024**。

这些是容量上限，而不是“把每个可用名额都派出去”的建议。管理者应当使用
最小可用的扇出规模，为扇入保留单一所有者，并在报告合并完成之前核实
worker 的回执。

RLM 的子查询批处理属于另一种更便宜的成本类别。它的 `sub_query_batch`
辅助函数在活跃的 `rlm` 会话内接受 1–16 个一次性子级；它不能替代携带工具的
`agent` worker。

## 人类检查：`/tools`（`/tool-studio`）

`/tools` 渲染针对某个 `(turn, step)` 准备好的请求中工具字段的
**只读、有界的人类投影**。它不是第二个注册表，也不是执行表面。

**接缝。** 快照在 `MessageRequest` 构造完成后立即于
`crates/tui/src/core/engine/turn_loop.rs` 中、基于 `request.tools` 构建——
也就是交给模型客户端的同一个值。引擎在 `engine.rs` 中一次性解析周边的每回合数据
（`ToolSurfaceContext`：扁平化的注册表事实、MCP 池自己的服务器归属、
引擎注入的目录名称，以及已解析模型客户端的回执），并以纯数据传入，
因此每步的接缝绝不会重新锁定 MCP 池或持有工具对象。

**回合与步骤身份。** 一个回合的不同步骤之间工具集可能不同，所以每个快照都带上
回合 id 和步骤的标记，每个接缝各自输出自己的快照。TUI 只保留最新的那个
（`SessionState.last_tool_request_snapshot`）。在第一个接缝之前没有快照，
`/tools` 会直说这一点，而不是在 UI 里重建一个注册表。

两类事实被分开保留：

- **传输事实**来自准备好的请求：名称、描述、schema、`defer_loading` / `strict` /
  `allowed_callers` / `cache_control`、字节统计，以及目录摘要。
- **表面事实**来自 `ToolSurfaceContext`：来源（`builtin` / `plugin` / `mcp` /
  `synthetic` / `unknown`）、MCP 服务器身份、声明的能力、声明的审批要求，
  以及模型可见性。

契约：

- **一个摘要。** `active_tool_catalog_sha256`
  （`crates/tui/src/core/engine/preview.rs`）是激活工具目录哈希的唯一定义。
  请求清单把它发布为 `ToolSurfaceFacts::active_tool_catalog_sha256`，
  而 `/tools` 对同一个准备好的请求报告相同的值；两个表面都不自己留一份哈希。
- **什么都不猜。** 只有当真实的池把那个确切的模型工具名称归属出来时，
  才显示 MCP 服务器身份。`McpPool::mcp_model_tool_name` 是模型目录与人类归属
  共享的唯一定义，而一个有歧义的名称（两个服务器在一个模型名称上撞车）
  会解析为没有服务器。合成来源来自 `default_synthetic_catalog_tool_names`，
  它会对照引擎自己的 `is_synthetic_catalog_tool` 谓词做断言。
  没有注册表条目的已传输工具会报告 `capabilities: unknown`，绝不用“none”。
- **提供商可用性跟随已解析的客户端。** 它来自
  `Engine::tool_surface_provider_receipt`，绝不来自“存在一个工具注册表”。
  没有客户端时，即使注册表是满的，回执也是 `unavailable`。
- **“未知”会收缩，但不会消失。** `unavailable_for_this_request` 始终包含
  `provider_wire_payload`：这条路径上没有任何东西观察到提供商适配器最终传输了什么。
  在没有已解析客户端时，它还会包含 `provider` 和 `model`；在没有捕获到表面上下文时，
  还会包含 `provenance` / `capabilities` / `approval`。
- **缺失与空保持区分。** 没有工具字段的请求，不等于工具数组为空的请求；
  未解析的字段是带原因的 `unknown`，而不是某个默认值。
- **有界。** 渲染受工具数量（32）、名称、描述、schema 字节数、允许调用方数量
  以及载荷测量上限的约束，每一项都有明确的截断或省略回执。这个请求*不*携带的
  已注册工具，会以有界名称列表加确切数量的形式报告，而不是把投影展开。
- **惰性。** 快照放在转录旁边，绝不放进 `session.messages`，因此它无法进入
  模型请求，也不会扰动提供商的提示前缀缓存。它从不执行工具，从不读取凭据，
  从不重排目录，也从不被注册为模型可调用的工具。
- **绝不声称已投递。** 捕获发生在连接建立之前，所以 `delivery_status`
  保持为 `unknown`。

## 发布验证

不要从处理函数名称推断公开表面。请在确切的候选 SHA 上核实模型目录与别名可见性：

```bash
python3 scripts/measure-runtime-contract.py
cargo test -p codewhale-tui --lib --locked core::engine::tests::default_active_contract_keeps_discovery_and_core_tools_eager -- --exact
cargo test -p codewhale-tui --lib --locked tools::file_tool::tests::primitive_schemas_are_separate_and_small_contract_shaped -- --exact
cargo test -p codewhale-tui --lib --locked tools::shell::tests::lowercase_bash_schema_is_small_contract -- --exact
cargo test --locked -p codewhale-tui --lib core::engine::tests::print_mode_tool_catalog_metrics -- --ignored --exact --nocapture
```

在相信一次绿色运行之前，先把测试名称与源代码核对：当一个过滤器没有匹配到
任何东西时，`cargo test` 会以“0 passed; N filtered out”退出并返回 0，
所以拼错的过滤器与通过是难以区分的。上面每条 `--exact` 命令都必须报告
`1 passed`（被忽略的指标测试报告 `1 passed` 只是因为 `--ignored` 选中了它）；
`0 passed` 意味着过滤器没匹配到任何东西，检查根本没跑。

不依赖提供商的回执必须报告上面列出的十二个默认激活名称。另一份仓库范围的
工具计数可能包含延迟、动态、受特性开关控制以及仅为兼容而存在的注册；
它不是放进首回合模型目录的工具数量。


## 源码导航回声

文件和搜索工具可在工具结果历史中附带同目录伙伴、符号、导入和修改归因，不加入会话固定前缀。调用者扫描最多选择 32 个可见、排序后的候选文件，每个正文最多接受 256 KiB，超额候选明确标为未扫描。关键文件正文最多接受 8 MiB。受保护路径或拒绝结果不会作为“读取不存在”继续探测；读取通过已验证的常规文件句柄完成。删除按旧侧、替换按两侧归因。完整目录名称枚举和内核 I/O 延迟不受正文上限约束。见英文 [ECHOLOCATION.md](../ECHOLOCATION.md) 及 [TOOL_SURFACE.md](../TOOL_SURFACE.md)。
