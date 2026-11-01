# 共享命令/控制平面契约

> 英文原文：[COMMAND_CONTROL_PLANE.md](../COMMAND_CONTROL_PLANE.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

Issue #1888 和 #4022。

Codewhale 在三个表面上暴露同一套生命周期操作：输入到输入区（composer）的斜杠命令、
绑定的热栏（hotbar）槽位，以及一个 CLI 入口点。在这份契约之前，这三个表面可能——也确实——
发生了漂移：`/fleet status` 显示的是当前会话的子智能体（subagent），而
`codewhale fleet status` 读取的是持久账本；CLI 的 Lane 动词则完全没有对应的斜杠命令。

契约就是一张类型化的描述符表，加上每个域一个执行器，位于
[`crates/lane/src/control.rs`](../../crates/lane/src/control.rs) 和
[`crates/engine/src/fleet/control.rs`](../../crates/engine/src/fleet/control.rs)。
`codewhale-lane` 是薄 CLI 门面和 TUI 都已经依赖的最低层 crate，
所以契约放在这里，既只有一份，又不必分叉。

## 词汇

以下定义不变且承重：**Fleet = 谁**，**Workflow = 顺序**，**Lane = 一个运行中的 Workflow**，
**Runtime = 在哪/如何**。Auto-Review 是一种权限档位（posture），绝不是评审角色。
没有“Operation”这个产品名词；内部的 `ControlOperation` 类型命名的是控制平面*动词*，
绝不出现在面向用户的文案里。

## 描述符钉住了什么

每个 `(domain, verb)` 对都有且只有一个 `OperationDescriptor`，以 `<domain>.<verb>`
形式的稳定 id 为键：

| 字段 | 含义 |
| --- | --- |
| `id` | `lane.status`、`fleet.interrupt`……——在每个表面上、每份回执（receipt）里都是同一个字符串 |
| `authority` | `read` 或 `write`。这不是权限档位：它说明该动词是观察持久状态还是改变持久状态 |
| `persistence` | 效果落在哪个持久存储（`lane_registry`、`fleet_ledger`） |
| `target` | 它作用于哪个确切的身份（`none`、`lane_run`、`fleet_worker`、`fleet_run`） |
| `retry` | `idempotent` 或 `unsafe` |
| `surfaces` | 哪些表面提供它 |
| `backend` | `Implemented`、`NotImplemented { hint }` 或 `SurfaceLimited { available_on, hint }` |
| `slash_command` / `cli_invocation` | 确切的绑定；热栏 action id 始终是 `slash.<slash_command>` |

当前的动词表：

| 动词 | Lane | fleet |
| --- | --- | --- |
| `list` | 读，整个注册表（registry） | 读，整个账本 |
| `status` | 读，一个 Lane | 读，整个账本 |
| `interrupt` | 写，一个 Lane（幂等） | 写，一个 worker（幂等） |
| `restart` | **无后端**——Lane 是被重新创建，而不是重启 | 仅 CLI（驱动管理器循环） |
| `resume` | **无后端**——已停止的 Lane 其 Runtime 会话已经消失 | 写，一次运行（幂等） |

## 没有表面会宣传自己做不到的事

`OperationDescriptor::availability(surface, ctx)` 返回 `Available`，
或返回带净化后提示的类型化 `UnavailableReason`：

- `backend_not_implemented`——没人实现过它。所有表面都拒绝。
- `surface_not_supported`——后端存在，但不在这里。提示会指出可用的那个表面
  （`codewhale fleet restart <worker-id>`）。
- `no_lane_registry` / `no_fleet_ledger`——持久存储还不存在。

可用性探测是**只读**的。`LaneRegistry::open_default` 和 `FleetManager::open`
都会顺带创建自己的存储，所以状态类动词会先探测 `lane_registry_root()` /
`fleet_ledger_path()`。否则“这个工作区没有 fleet 账本”就会悄悄变成
“这是我刚创建的一个空 fleet 账本”。

## 精确的运行身份

`parse_target` 是三个表面共用的唯一目标解析器：只接受一个 token、只接受精确 id
（没有前缀匹配或模糊匹配）、允许 ASCII 字母数字加 `-`、`_`、`.`，不允许路径分隔符，
并且当无目标动词被传入参数时硬性拒绝。

写入可以通过追加 `@<lifecycle-seq>` 来**加栅栏**：

```
codewhale lane interrupt lane-a1b2c3d4@3
/lane interrupt lane-a1b2c3d4@3
```

如果持久记录已经越过序号 3，该动词会以 `conflict` 失败并给出所观察到的序号，
而不是去停止此刻碰巧在那里的对象。

## 回执

每次调用都返回一个 `ControlReceipt`，携带操作 id、表面、权限、持久化作用域、可用性、目标、
`LifecycleOutcome`（`inspected`、`transitioned`、`no_change`、`rejected`、`failed`）、
所观察到的生命周期序号、可重试性、可选的有界净化失败信息，以及可选的有界运行分页。
`ControlReceipt::render()` 是唯一的渲染器；CLI 打印它，斜杠命令把它作为消息返回。
Lane 动词上的 `--json` 输出的也是同一个结构体。

## 类型化的未知

运行 DTO 绝不暗示“不存在”。`Known<T>` 要么是 `Known(value)`，要么是 `Unknown(reason)`，
其中 reason 为 `not_recorded`、`not_applicable` 或 `redacted`，并且渲染为 `<not_recorded>`，
而不是空白或看似合理的默认值。

具体来说：fleet 回执的 `FleetResolvedRoute` 只记录**生效**的思考档位（reasoning tier），
所以 `requested_reasoning` 是 `not_recorded`——它不会用生效值回填，
`reasoning_downgraded()` 返回 `None` 而不是猜测。Lane 注册表完全不记录路由或用量，
因此那些字段一律是 `not_recorded`。fleet 运行是按任务、而非按运行加栅栏，
所以 fleet 运行的 `lifecycle_seq` 是 `not_applicable`。

## 边界与脱敏

- 运行列表是分页的：`DEFAULT_RUN_LIST_LIMIT`（50），硬上限 `MAX_RUN_LIST_LIMIT`（200），
  并且分页会报告 `total` 和 `truncated`，这样一个边界永远不会被误认为空结果。
- 状态 worker 行和检视工件（artifact）行上限为 24，并带显式的省略提示。
- 回执详情上限为 `MAX_DETAIL_LINES`（40）行，每行 `MAX_DETAIL_LINE_CHARS`（240）个字符。
- 每个对操作者可见的字符串都要过 `sanitize_line`：以 `$HOME` 为根的路径折叠为 `~/…`，
  形似凭据的 `key=value` 对和已知的 token 前缀（`sk-`、`ghp_`、`xoxb-`、`Bearer`……）
  变成 `[redacted]`。

## 模型可见的工具表面

未变。这项工作不新增任何工具、任何工具参数、任何提示词文本；面向模型的子智能体表面仍然只有
`agent`。不需要做工具 schema 的回归度量。

## 测试

- `crates/lane/src/control.rs`——描述符表的完整性、两个域上五个动词的对称性、
  跨表面的 authority/persistence/target 一致性、可用性规则、目标解析与生命周期栅栏、
  回执往返、边界限制与脱敏；另有执行器测试，证明三个表面针对同一个持久 Lane
  得到逐字节相同的结果，以及 interrupt 是幂等且带栅栏的。
- `crates/engine/src/fleet/control.rs`——带类型化未知的路由/用量 DTO 投影、有界分页与行、
  在账本缺失时如实报告而不创建、仅 CLI 的 `fleet.restart`，以及 `fleet.status`
  的跨表面身份一致。
- `crates/tui/src/commands/groups/core/lane.rs` 和 `…/fleet.rs`——斜杠动词映射到共享操作，
  `/fleet status` 读取持久账本而不是会话子智能体，并且裸派发（热栏触发的形式）是只读的。
- `crates/cli/src/lib.rs`——CLI 在相同的 id 下恰好暴露所声明的 Lane 动词，
  且 `lane stop` 是 `lane interrupt` 的兼容写法。
