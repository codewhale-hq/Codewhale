# 运行时 API 与集成契约

> 英文原文：[RUNTIME_API.md](../RUNTIME_API.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。
> 2026-10-06 补齐设备令牌意图、逐调用变更、技能详情、持久配置与账号宪章契约。
> 2026-10-11 补齐工作区信任契约：`workspace_trusted` 与 `POST /v1/workspace/trust`。
> 2026-10-23 补齐完整 API 入口、兼容执行限制与 ACP MCP 拒绝边界。

`codewhale app-server --http` 是本地运行时的规范 API 与控制面。本地 SDK、移动端/远程控制客户端
以及编辑器集成都与它对话，而不是去抓取终端输出。它提供完整的 HTTP/SSE 运行时
API（`/v1/*`）、基于 stdio 的 JSON-RPC 控制传输，以及面向手机的移动端页面。
`codewhale doctor --json` 提供机器可读的健康状态，`codewhale serve --acp` 则通过 stdio
使用 Agent Client Protocol 与 Zed 这类编辑器对话。

`codewhale serve --http` / `serve --mobile` 仍作为 `codewhale app-server --http` /
`--mobile` 的**兼容别名**存在；两者启动的是同一个服务器。新的集成应当以
`app-server --http` 为目标。裸 `app-server`、`--stdio` 和 `--socket` 是能力较小的兼容接口。

`codewhale exec` 是另一条一次性的无头 worker 路径（stream-json、fleet worker 子进程、
CI 原语）。它不属于本 API，但共享同一个运行时、提供商/模型解析、权限配置
与事件词汇表。

本文档是原生工作台应用（以及其他本地监督进程）嵌入 Codewhale 引擎的稳定集成契约。

## 架构

```
local supervisor / SDK / automation harness
        │
        ├─ codewhale app-server --http     → HTTP/SSE runtime API (/v1/*)        [canonical]
        ├─ codewhale app-server --mobile   → runtime API + mobile control page
        ├─ codewhale app-server --stdio    → JSON-RPC control transport over stdio
        ├─ codewhale app-server --socket   → same JSON-RPC over a unix domain socket (desktop daemon)
        ├─ codewhale doctor --json         → machine-readable health & capability
        ├─ codewhale serve --acp           → ACP stdio agent for editors such as Zed
        ├─ codewhale serve --mcp           → MCP stdio server
        ├─ codewhale serve --http/--mobile → legacy aliases for `app-server --http/--mobile`
        └─ codewhale exec [args]           → one-shot headless worker (stream-json)
```

引擎以仅限本地的进程运行。所有 API 默认绑定到 `localhost`。没有托管中转，
不代管提供商令牌，也不泄漏任何密钥。

关于线程或回合所做之事的只读记录，请参阅
[`docs/RECEIPTS.md`](../RECEIPTS.md)：CLI 上的 `codewhale receipts`，以及下文
**线程**下的 `/receipt` 路由。

## 运行时 API 入口

| 入口 | 传输 | 用途 |
|---|---|---|
| `codewhale web [--port 7878]` | 在 `127.0.0.1:7878` 上的 HTTP/SSE + 内嵌客户端 | 一等公民的仅回环浏览器客户端；打开默认浏览器 |
| `codewhale app-server --http` | 在 `127.0.0.1:7878` 上的 HTTP/SSE | 完整的 `/v1/*` 运行时 API（规范入口） |
| `codewhale app-server --mobile` | 回环上的 HTTP/SSE + `/mobile` | 运行时 API + 本地移动端控制页 |
| `codewhale app-server --stdio` | 基于 stdio 的 JSON-RPC 2.0 | 本地 SDK / 控制探针（不监听端口） |
| `codewhale app-server --socket [--socket-path P]` | 基于权限为 `0600` 的 Unix 域套接字（macOS/Linux）或仅限所有者的命名管道（Windows）的 JSON-RPC 2.0 | 桌面守护进程：多客户端、内核对端身份校验、`daemon/attach` 认领握手。Windows 原生应用验收仍待完成。 |
| `codewhale app-server` | 在 `127.0.0.1:8787` 上的 HTTP | 兼容 `/healthz`、`/thread`、`/app`、`/prompt`。回合请求必须显式带 `allowed_tools: []`；交互执行在回合开始前拒绝。工具、审批、澄清、引导和完整事件日志请用 `app-server --http`。 |
| `codewhale serve --http` / `--mobile` | 与 `app-server --http`/`--mobile` 相同的服务器 | 兼容别名 |

`app-server --http` 与 `--mobile` 启动的是历史上经由 `serve --http` 访问的同一个
成熟运行时 API 服务器——路由与行为都没有变化，因此下文记录的每个端点在这两个入口上
都完全一致。运行时 API 令牌按 `--auth-token`、`CODEWHALE_RUNTIME_TOKEN`、
`DEEPSEEK_RUNTIME_TOKEN` 的顺序读取；只有在绑定回环地址时才能使用
`--insecure-no-auth`。`serve` 兼容别名保留各自的 `--insecure` 标志。
旧式的进程内 `codewhale app-server` 在绑定非回环主机之前，同样要求显式的
`--auth-token` 或 `CODEWHALE_APP_SERVER_TOKEN`；它生成的一次性 `cwapp_*` 令牌
只能用于回环。

### 设备令牌的 Watch / Drive 意图

主令牌通过 `POST /v1/auth/client-tokens` 铸造的设备令牌有不可变的 `intent`：
省略时为 `watch`，控制需显式指定 `drive`。标签不授予权限。Watch 可读取普通
GET/HEAD、查看 Computer 显示并申请一次性显示票据；不能修改 Runtime 状态、
把受保护的 HTTP 读取升级成写通道、取得/释放控制或转发输入。
Drive 保留现有 Runtime/控制权限，但不能铸造、列举或撤销设备令牌。
显示票据保留签发主体的意图；输入仍需该精确主体当前有效的控制租约。

`GET /v1/runtime/info` 宣告 `capabilities.client_token_intents: true`，
铸造回执返回 `device_id`、`intent` 和 `expires_at`。Relay 签发者必须先确认此能力，
再核对回执与请求的设备/意图相同，才可暴露令牌。旧 Engine 没有此强制边界，
签发者必须拒绝为其发放 relay grant；意图式标签或旧版铸造成功不足以证明权限。
这不为 Engine 本地设备令牌添加账号/Computer 所有权范围。

### 工作区文件建议

`GET /v1/workspace/files/search?query=runtime&limit=20` 通过既有的、需认证的
`/v1/*` 路由返回 `{"paths":["src/runtime.rs"]}`。它只搜索服务器配置的工作区，
不搜索线程的工作区或进程的当前目录。不接受任何工作区/路径覆盖参数。
响应包含以 `/` 分隔的工作区相对文件路径，绝不包含文件内容、绝对路径或目录。

- `query` 是字面的文件名/路径片段，不带 `@` 前缀，最多 256 个 UTF-8 字节。缺失、
  为空或只含空白的查询会在不遍历文件系统的情况下返回空列表。没有匹配项时同样
  返回空列表。
- `limit` 默认为 20；可接受的值是 1–100。非法的 limit、超长的查询以及未知的查询参数
  都返回 HTTP 400。
- 匹配复用 TUI 模糊 `@file` 发现/排序：先按大小写不敏感的路径前缀匹配，再按子串匹配，
  每组内按字母顺序排列。这不是 glob、子序列、内容或语义搜索，也不应用 TUI 的
  个人 frecency 加权。
- 发现过程共享输入区的忽略策略，包括 `.ignore` 与 `.deepseekignore`、始终可发现的
  AI 目录，以及有界的、被隐藏或被 gitignore 的本地引用兜底。与 TUI 中一样，
  对 `.agents`、`.claude`、`.cursor`、`.deepseek` 的特殊遍历会有意绕过忽略规则。
  忽略文件不是保密边界。
- 不遍历目录符号链接。文件在被应用结果上限之前先做规范化并过滤，确认其处于工作区之内；
  指向外部的符号链接与失效的文件符号链接都会被忽略。工作区内的文件符号链接
  可以按其相对名称出现。建议结果是文件系统的一份快照，并不构成之后读取某个文件的授权；
  消费方在打开文件时必须重新校验。

发现过程在异步执行器之外运行，使用共享的默认深度 10、最多 20,000 个候选，
以及一个协作式的两秒发现预算。结果是最尽力而为的，不是穷尽列表；一次缓慢的文件系统
操作可能在该预算之后才完成。每次请求都会重新扫描；没有新的索引或缓存。
这个只读端点不会改动会话，也不会改动被固定下来的模型提示词/工具前缀。

### 工作区文件与会话工件

原生客户端通过三条需认证的路由浏览并编辑
服务器配置的工作区。它们直接读写工作区；没有第二套文件存储、缓存或索引，
也没有路径覆盖：工作区根目录是唯一的根目录。

- `GET /v1/workspace/files?path=<dir>&limit=<1-2000>` 列出单个目录。
  `path` 是工作区相对路径，以 `/` 分隔；空值或 `.` 表示根目录。
  每个条目携带 `name`、`path`、`kind`（`file`、`directory`、`symlink`、
  `other`），文件还携带 `size` 与 `modified`（RFC 3339）。目录排在最前，
  然后名称按大小写不敏感的顺序排列。`limit` 默认为 200；`truncated` 报告是否被截断。
  `.git` 永不被列出或提供，符号链接只按名称列出：它们永不被跟随，因此
  `path=<link>` 返回 403。
- `GET /v1/workspace/files/read?path=<file>&offset=<bytes>&limit=<1-4194304>`
  返回一个常规文件的一个字节窗口，并给出 `size`、`revision`（**整个**文件的
  SHA-256 十六进制值，不是该窗口的）、`modified`、
  `offset`、`bytes`、`truncated`、`encoding` 与 `content`。文本窗口为
  `utf-8`；含 NUL 字节、非法 UTF-8 或截断多字节字符的窗口为 `base64`。
  `limit` 默认为 256 KiB。大于 16 MiB 的文件以 413 拒绝；目录是 400；
  链接是 403；文件缺失是 404。
- `PUT /v1/workspace/files`，请求体为 `{"path", "content", "encoding"?,
  "expected_revision"?}`，经由 Fleet 工件所使用的同一个受限打开器
  原子地写入一个文件。`encoding` 为 `utf-8`（默认）或 `base64`；
  超过 4 MiB 的请求体返回 413。新建文件**不需要**
  `expected_revision`（并会在工作区内创建缺失的父目录）；覆盖写入则要求
  提供编辑所依据的那次读取得到的 `revision`，过期或缺失时返回 409，
  并在错误消息中带上当前 revision，以便客户端重新读取并合并。这是乐观并发，
  不是锁：两个写入者在检查与写入之间竞争时仍可能交错。响应携带 `path`、`size`、
  `revision`、`created` 与 `written_at`；新文件返回 201，其他情况返回 200。
  经由链接写入、写入 `.git`、或写入目录都会被拒绝。

每个路径在任何文件系统访问之前都会被校验：绝对路径、
反斜杠、`.` 或 `..` 组成部分都返回 400，并且沿途的每一级目录都在不跟随链接的前提下
打开（Unix 上逐组件 `O_NOFOLLOW`，Windows 上检查 reparse point）。这些路由与其他
任何 `/v1/*` 路由一样使用运行时 bearer 令牌；它们不查询模型的工具权限姿态，
因为调用方是已认证的操作者，而不是模型。

会话工件是会话记录为 `ArtifactRecord` 的超大工具输出
（`crates/tui/src/artifacts.rs`），存储在
`sessions/<id>/artifacts/` 下：

- `GET /v1/sessions/{id}/artifacts` 列出某个已保存会话携带的记录：
  `id`、`kind`、`tool_call_id`、`tool_name`、`created_at`、`byte_size`、
  `preview` 以及相对于会话的 `path`。
- `GET /v1/sessions/{id}/artifacts/{artifact_id}?offset=&limit=` 读取单个
  工件，使用与工作区文件读取相同的窗口、`revision` 与 `encoding` 契约。
  存储路径为绝对路径或逃出会话目录的记录返回 403；记录对应的文件已消失时返回 404。

这些路由只提供 SavedSession 索引到的内容（外加不可变的图片证据）。运行时回合的
溢出（spill）通过下文的回合路由读取。未绑定的运行时线程，其引擎根本没有
SavedSession 索引，所以对它的溢出而言，回合记录是唯一入口。

#### 回合工件

回合把它产出的东西，以类型化引用的形式记录在它的条目和回合自身上。构建这些引用
不做任何扫描。每条事实都记录在写入字节的地方：
- 文件工具和 `apply_patch` 在 `mutation.files[]` 中报告 `size`/`sha256`；
- 溢出报告 `artifact_digest`；
- 工具媒体报告 `sha256`。

工作区层面的那一半来自回合自己的恢复点：`TurnRecord.workspace_snapshots` 中的
`pre_turn` 与 `post_turn` 回执（见下文“工作区恢复点”），在已有的 side 仓库里按
tree id 做 diff。不涉及第二个存储、快照或事件。

一个引用（`TurnArtifactRef`）携带：

| 字段 | 含义 |
| --- | --- |
| `id` | 在回合内稳定。文件的 id 是 `file_` 加上 SHA-256(path) 的前 32 位十六进制数字。溢出的 id 是 `art_<call>`。媒体的 id 是 `art_image_<sha256>`。 |
| `kind` | `file`、`tool_output` 或 `media`。 |
| `path` | 对 `file`：工作区相对路径，使用 `/` 分隔。对 `tool_output` 与 `media`：会话相对路径（`artifacts/...`）。 |
| `change` | 仅对 `file`：`created`、`updated`、`deleted` 或 `renamed`。重命名还带有 `previous_path`。 |
| `size` | 字节大小。文件已删除时不存在。 |
| `revision` | 整个内容的 SHA-256 十六进制值。它就是 `GET /v1/workspace/files/read` 报告为 `revision` 的值，而 file-revert 的 `expected_hash` 是 `sha256:` + `revision`。文件已删除时，或 delta blob 超过 16 MiB 时不存在。 |
| `content_type` | 对 `media`：确切的媒体类型。 |
| `session_id` | 对 `tool_output` 与 `media`：拥有这些字节的工件会话。 |
| `item_id`、`tool_call_id`、`tool_name` | 写入它的那次工具调用。只在工作区 delta 中看到的改动没有这些字段。 |
| `source` | `tool_mutation`、`tool_output_spill`、`tool_media` 或 `workspace_changed_during_turn`。 |
| `restore_snapshot_id` | 一个 `POST /v1/threads/{id}/file-revert` 在此线程上对该路径接受的恢复点：记录在本回合 `workspace_snapshots` 上的某个回执的 `tree_id`，因此无论线程是否绑定到已保存会话，它都归该线程所有。对工具写入，它是该调用的 `tool` 回执；对 delta 改动，它是回合的 `pre_turn` 回执。回合没有记录此类回执时不存在。 |
| `recorded_at` | 记录该引用的时间。 |

引用出现在哪里：
- **条目。** `TurnItemRecord.artifacts` 列出一次工具调用产出的东西。它在调用
  完成时设置（无论成功或失败），所以 `item.completed` 与 `item.failed` 会实时携带它。
- **旧版投影。** `artifact_refs` 由 `artifacts` 派生。它只保存仍然存在的文件的
  工作区相对路径：从不包含溢出、媒体或已删除的文件。
- **回合。** `TurnRecord.artifacts` 是回合汇总。它由一次合并计算得出，
  按最新在前排序。
  - 溢出与媒体引用总是保留。
  - 文件引用按条目顺序组合：先创建后删除则去掉该文件，先创建后更新仍为
    `created`，重命名会折叠它的来源。
  - 工作区 delta 一旦结算，就对快照能看到的每个路径的净改动、`size` 与
    `revision` 具有权威。它会补上没有任何工具回执点名的文件，例如 shell 和
    子智能体的写入。对于快照跟踪到、但在回合结束时未改变的条目路径，它会去掉。
  - 汇总上限为 1000 个引用，`workspace.truncated` / `workspace.omitted`
    报告截断情况。

`TurnRecord.workspace` 跟随 delta 的生命周期。回合运行期间它是 `null`。
`turn.completed` 携带它，状态为以下之一：
- `pending`：回合同时记录了 `pre_turn` 与 `post_turn` 回执，它们的 diff 仍在
  运行。完成后，运行时发布 `turn.artifacts`
  （`{turn_id, workspace, artifacts}`）。该事件可能在下一个回合的
  `turn.started` 之后才到达，所以请按 `turn_id` 关联。
- `settled`：delta 已合并。`pre_turn_snapshot_id` 与 `post_turn_snapshot_id`
  是这一对快照的 tree id。
- `unavailable`：不会有 delta。`reason` 说明原因：
  - `snapshots_disabled`、`workspace_too_large`、`too_many_files`、
    `unsafe_location`、`snapshot_failed`：快照门槛。有 `pre_turn` 回执但没有
    `post_turn` 回执的回合保留 `pre_turn_snapshot_id`。
  - `not_captured`：回合没有记录恢复点：压缩或清除操作，或引擎在拍快照之前
    就结束的回合。
  - `runtime_restarted`：进程在 delta 结算前停止。这会在启动时对账，永不重新计算。
  - `delta_failed`：diff 本身失败。

  `artifacts` 仍保存工具回执记录的内容。`turn.artifacts` 会为每一种结算结果发布，
  所以等待 `pending` 的客户端总能收到回音。

delta 意味着什么，以及它看不到什么：
- **它是工作区 diff，不是归属。** delta 改动是回合运行期间工作区里改变的一切。
  这包括同时写同一工作区的编辑器、另一个线程或后台任务。
  `source: workspace_changed_during_turn` 说的正是这一点。
- **被排除的路径对快照不可见。** 这包括内置排除项（例如 `node_modules/`、
  `target/`、`dist/`、`build/`、`.next/`，以及二进制与媒体扩展名）和工作区的
  `.gitignore`。文件工具写入此类路径时，仍会根据其回执报告。shell 命令写入
  此类路径时，则完全不会报告。
- **shell 写入没有逐调用记录。** shell 命令的写入永远不会归到它的条目，只会
  归到回合，而且只在启用快照时。

路由：

- `GET /v1/threads/{id}/turns/{turn_id}/artifacts` 从运行时存储的回合记录返回
  `{thread_id, turn_id, workspace, artifacts}`。回合运行期间，`artifacts` 由其
  条目即时合并，`workspace` 为 `null`。未知回合，或属于另一个线程的回合，返回 404。
- `GET /v1/threads/{id}/turns/{turn_id}/artifacts/{artifact_id}?offset=&limit=&revision=`
  读取一个引用。
  - 它使用工作区文件读取的窗口契约（`size`、`revision`、`offset`、`bytes`、
    `truncated`、`encoding`、`content`），并额外添加：
    - `artifact`：该引用；
    - `source`：`workspace`、`snapshot` 或 `session_artifact`；
    - `current`：工作区是否仍持有这些字节；对该引用而言这不是一个问题时为 `null`。
  - `revision` 选择回合的某个条目记录过的中间修订。默认是该引用自己的修订。
  - 当工作区仍持有记录的修订时，`file` 从工作区提供（`current: true`）。
    否则来自回合的 post-turn 快照（`current: false`）。
  - `tool_output` 或 `media` 引用在写入方使用的会话工件根下读取。适用与会话
    路由相同的限制、图片清单与完整性检查。

| 状态 | 何时 |
| --- | --- |
| 404 | 未知的线程、回合或工件 id，或本回合从未记录过的 `revision`。 |
| 409 | 文件记录的修订既不在工作区也不在快照存储中（快照在每个工作区超过 50 个或 7 天后被修剪），或会话工件的字节不再哈希为记录的修订。消息会给出当前修订。 |
| 410 | 回合删除了该文件（用 `file-revert` 和 `restore_snapshot_id` 恢复它），或会话工件的字节已被修剪。 |
| 413 | 内容超过 16 MiB。 |
| 403 | 符号链接，或逃出其根的引用。 |

Fleet 回执工件保留自己的路由
（`GET /v1/fleet/runs/{run_id}/receipts/{task_id}/evidence`）。

### 一个工具调用实际改了什么

`GET /v1/threads/{id}/turns/{turn_id}/calls/{tool_call_id}/changes?limit=`
从同一回合的 `tool:` / `post-tool:` 两个恢复点读取逐调用变更。Shell 自己写的文件
也会出现；它没有 `metadata.mutation`，不能仅靠文件工具事件归因。
响应含 `thread_id`、`turn_id`、`tool_call_id`、`tool_name`、`state`、`reason`、
`truncated` 和 `files`。每个文件有 `path`、`change`、`added`、`removed`、`size`、
`revision`、`restore_snapshot_id`、`diff` 与 `diff_truncated`。

- `change` 为 `created`、`updated` 或 `deleted`；类型变更是 `updated`，
  移动是删除加创建。二进制文件的行数是 `null`。
- `size` / `revision` 属于区间末尾，绝不从当前工作树猜测。`revision` 是裸
  SHA-256；传给 `file-revert` 时用 `sha256:<hex>`，该调用删除的路径用 `absent`。
  删除或文件过大无法读取时，`size` / `revision` 为 `null`。
- `diff` 在字符边界截到 64 KiB。二进制、无内容差异或无补丁时为 `null`。
- `pending` 表示调用排队/执行中，快照对未完整；`files` 为空，结算后再读取。
  `captured` 也可能是空文件列表，表示完整记录的调用没有改动。
- `unavailable` 的 `reason` 可以是 `call_not_bounded`、`post_snapshot_missing`、
  `pre_snapshot_missing` 或 `snapshots_pruned`。这不是“没有改动”：回执中的
  `changed_paths` 仍可读取，但旧快照可能已被修剪或删除。
- `limit` 默认 200、最大 1000；被截断时 `truncated` 为真，不返回省略数量。
  读取只在旁仓执行 `git diff`，不更改工作树或索引。

未知线程/回合、别的线程的回合或没有对应记录的调用返回 404；非法 `limit`
返回 400；记录读取/解析或快照仓操作故障返回 500，不能当作快照已被修剪。

### 运行时与账号身份

`GET /v1/runtime/info` 报告 `codewhale_version`，以及由共享的 CLI/TUI 构建嵌入的
完整 40 字符 `codewhale_commit`。无法提供精确 commit 的源码归档会报告 `unknown`，
让兼容客户端可以按失败即关闭（fail closed）处理，而不是接受一对含义不明的二进制组合。

同一响应还会声明 `capabilities.account_session: true` 与
`capabilities.turn_operation_idempotency: true`。客户端在依赖 `operation_key`
之前必须要求后者；不要因为回合返回了 2xx 就推断其受支持，因为一个较旧的宽容读取方
可能忽略未知的请求字段。`capabilities.turn_operation_lookup: true` 单独声明
下文那个只读操作查询；客户端在依赖基于 GET 的丢失回合响应恢复之前必须要求它。
响应中还包含一份不含令牌的账号回执：

```json
{
  "account": {
    "schema_version": 1,
    "state": "authenticated",
    "api_base": "https://api.codewhale.net",
    "account_id": "acct_...",
    "session_id": "session_...",
    "scopes": [],
    "expires_at": "2026-08-01T20:00:00Z"
  }
}
```

运行时从 `codewhale account login` 写入的那条精确的、按 profile 与 API 源站限定范围的
安全记录中读取这份回执；它不会再跑一遍登录流程。状态有 `signed_out`、`authenticated`、
`offline_cached`、`expired` 或 `revoked`。scopes 只从显式存储的会话授权中复制，
绝不从账号身份推断。访问令牌/刷新令牌、邮箱、提供商 profile 与提供商凭据
永不返回。只有在请求由运行时令牌（或显式开启的不安全回环服务器）授权时，
才会包含 `account_id` 与 `session_id`；公开的 bootstrap 响应仍然可用，但会报告
`signed_out`。未登录状态下的本地 Work 仍然受支持，并且绝不会隐式分配云端算力。

`--stdio` 控制传输是换行分隔的 JSON-RPC 2.0。可以在不消耗模型 token 的情况下探测它：

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"healthz"}' \
  '{"jsonrpc":"2.0","id":2,"method":"capabilities"}' \
  '{"jsonrpc":"2.0","id":3,"method":"shutdown"}' \
  | codewhale app-server --stdio
```

`capabilities` 返回已声明的方法族（`thread/*`、`app/*`、
`prompt/*`）以及完整的方法清单；`thread/capabilities`、
`app/capabilities` 与 `prompt/capabilities` 则按族缩小范围。方法集合由
`crates/app-server/src/tests.rs` 中的一个漂移测试钉住，因此 SDK 与本地集成客户端
可以依赖它不会悄悄变化。

### 兼容执行限制

HTTP 兼容路由、stdio 和 socket 均声明 `interactive_execution: false`、`goal_execution: false`、
`tool_free_turns: true` 与 `full_api_entrypoint: "codewhale app-server --http"`。
客户端必须先确认 `tool_free_turns: true`，才能依赖空工具列表；旧兼容服务器可能忽略该字段。
`thread/message`、`thread/request` 的 message、`prompt/run`、`prompt/request`
以及 HTTP `/prompt`、`/thread` 的 message 必须显式提供 `allowed_tools: []`。
省略、null 或非空列表返回 `interactive_execution_unsupported`，不会创建 Runtime
线程或启动回合；错误类型的值也会被拒绝。桥在实际回合请求中继续传递空工具列表，
只转发助手文本和最终状态；意外的审批或澄清请求会报错并中断该回合。
工具客户端须迁移到带 bearer 认证的完整 HTTP/SSE API。
JSON-RPC 返回 `-32602` 和 `data.error: "interactive_execution_unsupported"`；
HTTP 返回 400，`error.code` 为 `invalid_request`，同一详情位于 `error.data`。
兼容 `thread/goal/set` 以及 `ThreadRequest::GoalSet` 也在修改目标前拒绝，
因为设为活动目标会启动后台工作。读取、清除现有目标仍可用；开始目标请用完整 HTTP API。

`@codewhale/runtime-sdk` 当前只封装 Fleet 和线程事件读取，未封装线程创建、
回合开始/引导/中断、审批、澄清或模型发现。完整集成请使用
[SDK 指南中的认证 HTTP 示例](../../npm/runtime-sdk/README.md#full-engine-api)。

### 守护进程套接字：`codewhale app-server --socket`

此套接字传输在长期存活的守护进程上暴露兼容控制。
其线上协议就是 `--stdio` 传输本身——同样的换行分隔 JSON-RPC 2.0 方法，由同一份代码
分发——只是在前面加了一次握手。

**端点。** 若给了 `--socket-path` 就用它；否则若设置了 `CODEWHALE_HOME` 则用
`$CODEWHALE_HOME/run/daemon.sock`（显式指定 home 即是一条隔离边界）；否则用
`$XDG_RUNTIME_DIR/codewhale/daemon.sock`；再否则在 macOS 上用
`~/Library/Application Support/codewhale/daemon.sock`，其他平台用
`~/.codewhale/run/daemon.sock`。目录以 `0700` 创建，套接字为 `0600`，
每个被接受的连接方都必须出示守护进程自身的 uid。
启动时，无人应答的套接字文件会被删除；若有一个仍在工作的，则新守护进程以
`a live listener already answers on <path>; refusing to
replace it` 退出；路径上本就不是套接字的文件永不被触碰。在 Windows 上 `--socket` 会失败，
返回一个带类型的 `UnsupportedPlatform` 错误，并点名预留管道 `\\.\pipe\codewhale-daemon`
——没有静默的 TCP 回退。守护进程开始接受连接后会向 stderr 打印
`codewhale daemon: listening on <path>`。

**握手。** 一条连接上的第一个请求必须是 `daemon/attach`
（在此之前也允许 `healthz`，以便外壳探测存活）。在此之前，其他每个方法都会以
`-32010 attach_required` 被拒绝。

```json
{"jsonrpc":"2.0","id":1,"method":"daemon/attach","params":{
  "client":{"name":"codewhale-desktop","version":"1.2.3","pid":4242},
  "mode":"claim",
  "expect_daemon_version":"0.9.11"}}
```

`mode` 为 `"claim"`（该客户端拉起了这个守护进程并管理其生命周期）
或 `"attach"`（默认：一个发现了健康守护进程的访客）。当另一条连接已拥有该守护进程时，
claim 会以 `-32011 daemon_already_claimed` 失败
（`data.owner` 指明持有者），客户端应改用 `attach` 重试。
`expect_daemon_version` 若存在，必须等于守护进程的 crate 版本，
否则 attach 会以 `-32013 daemon_version_skew` 失败（插件包偏差防护）。
回复报告授予的 `role`（`owner` / `attached`）、守护进程的
`pid`、`version`、`socket_path` 与 `uptime_ms`、当前 `owner`，以及
活动 `connections` 数量。在一条已 attach 的连接上再次 `daemon/attach`
会返回 `-32014 already_attached`。

**能力。** 在这条传输上，`capabilities.methods` 是钉住的 stdio 集合加上 `daemon/attach`
（第二项，排在 `healthz` 之后）；`transport` 读作
`unix-socket`。`shutdown` 会向每条连接声明，因为该方法确实存在，但只有 owner 才能调用它
（见下文）。

**所有权。** 只有 owner 可以 `shutdown`；访客的 `shutdown` 会以
`-32012 not_daemon_owner` 被拒绝，并且不会打断任何人的回合。当
owner 断开连接时，这个位置就释放了，因此重新启动的外壳可以重新认领它此前留下继续运行的守护进程。
owner 的 `shutdown` 会停止监听、关闭所有
连接并删除套接字文件。从客户端最后见到的 `seq` 开始回放日志
目前还不属于这条传输。

### 中断一个回合

`thread/message` 会持续流式传输，直到回合进入终态，这可能
耗时数分钟。在回合流式传输期间，读取循环会继续轮询 stdin，因此
客户端可以发送：

```json
{"jsonrpc":"2.0","id":9,"method":"thread/interrupt","params":{"thread_id":"thr_..."}}
```

运行时会收到请求去中断该回合
（`POST /v1/threads/{id}/turns/{turn_id}/interrupt`）。当该线程没有回合在流式传输时，
回复会带上 `interrupted: false`——这不是错误，只是没有可停止的东西。被中断的
`thread/message` 随后会以 `turn interrupted` 错误失败，并且它的回复会写在
中断自身回复之前，因为在该回合回退（unwind）之前，写入方由它独占。

在一个进行中的回合期间发送 `shutdown` 也会先中断：它需要该回合持有的同一个
桥，否则它就会一直等待那个它本意要停止的回合。回合中途到达的其他请求会排队，
并在回合结束后按顺序执行。

### 运行一个提示词

`prompt/request` 与 `prompt/run`（字节级完全相同的别名）以及旧式
HTTP `POST /prompt` 都会在运行时上执行一个**真实回合**，经由
`thread/message` 使用的同一个桥。没有本地回退：app-server 中没有任何其他东西
能产生模型输出，所以一个提示词要么真的运行，要么就失败。

- `params.prompt` 是必需的，且必须非空（否则返回 `-32602`）。
- `params.thread_id` 是可选的。带上时，提示词在该线程及其
  历史上运行。不带时，运行时会为这一个回合拿一个全新线程；
  该回合结束时这层映射就被丢弃，因此一次性提示词无法
  通过 `thread/interrupt` 寻址。当你需要能够中断时，请使用 `thread/message`。
- `params.model` 只在本次调用正是创建运行时线程的那一次调用时选择模型；
  已有的线程保持它创建时所用的模型。
- 响应携带模型实际说出的内容：`output` 是串联起来的
  `agent_message` 文本，`model` 是运行时为运行该回合的线程报告的模型，
  `events` 则是真实的
  `response_start`/`response_delta`/`response_end` 帧。在 stdio 上，同样的
  帧还会在回合运行期间流式输出到 stdout，与
  `thread/message` 完全一致。
- 如果无法访问运行时，调用在 stdio 上以 `-32005`
  （`runtime_unavailable`）失败，或在 `POST /prompt` 上返回 HTTP `503` 与
  `{"error":{"code":"runtime_unavailable", ...}}`。失败
  绝不会被塑造成一个成功的 `PromptResponse`。

带 `Message` 请求体的 `POST /thread` 行为相同——它运行该回合
并回复 `status: "completed"`，`events` 中带上流式帧——而
它以前只是回复 `accepted` 而什么都不做。

### 线程标识与重启

规范线程 owner 保留完整已保存会话图、当前分支以及线程/会话绑定。
兼容控制接口使用同一个已认证 owner；旧 SQLite 历史只作为受保护的只读导入来源。
导入会在发布规范别名之前比较完整来源图和当前叶节点。别名发布失败时，
来源和已完成的规范结果都会保留，恢复时可以说明实际完成了什么。

已验证的旧目标导入同一个 owner 目标存储。导入时活动目标暂停；旧数据不会启动
提供商调用。来源目标字段参与同一个受保护的来源比较。

已保存会话的 fork 保留完整日志，包括非活动分支，并把已验证的本地会话目标
sidecar 复制到新会话。活动本地目标复制为暂停状态；来源保持不变。
该 sidecar 与公开的 Runtime 线程目标分开。原生 Runtime 线程 fork 不会自动
继承公开线程目标。

`thread/create`、`thread/start`、`thread/resume` 和 `thread/fork` 携带
客户端生成的 `operation_key`。每个用户意图在发送前生成一个键；响应不确定时
保留该键，并用它恢复同一个意图。Create 将键放在 `metadata.operation_key`；
Start、Resume 和 Fork 使用 `operation_key`。两次有意的 fork 使用不同键。恢复查询现有
owner 存储中的原始操作；响应丢失或来源历史后来增长不会创建另一个线程。

`POST /v1/thread-history/operations/lookup` 是只读查询。封闭请求包含
`version: 1`、`operation_key`、`expected_data_dir`、`expected_execution_scope`
和 `workspace`；响应为 `absent`、`pending` 或 `committed`。待决和已提交响应
包含保留的回执以及准确的操作/来源 `association`。

`POST /v1/thread-history/operations/recover` 在 `operation` 中接收该查询请求，
并接收预期的 `association`。它在同一个 owner 下验证已保存文档、完整图、
工作区、检查点和操作/来源身份，然后明确完成已准备好的目标。
它不会从可能已变化的来源重新构造原始意图。尚未准备好的目标保持待决；
变化或无法验证的目标拒绝完成。使用同一个键重复恢复会观察到同一个已提交结果。

选定工作区来自已确认的 owner 或明确获准的请求。历史和旧回执不能提供权限、
凭据、端点或另一个 owner。绑定存储缺失、变化、繁忙或不兼容时明确失败。
规范目标缺失不会启动一个空的替代会话。

`codewhale thread resume` 和 `codewhale thread fork` 执行持久化 owner 控制，
并输出已提交的线程、会话和操作回执。这两个命令不启动交互式界面。

全局 `--workspace`（也可用 `--cd`）、`--profile` 和 `--config` 选择明确的
控制范围。相对路径在挂接前确定；客户端先认证 owner，再使用同一个 owner
回执和已捕获的 worker 设置接纳该范围。不兼容的 profile 或配置、缺失的范围
信息或 owner 变化都会明确失败。省略这些选项时，工作区来自已确认的 owner。
线程列表仍覆盖整个存储。

新的 `thread resume` 或 `thread fork` 可通过全局 `--provider`、`--model`、
`--approval-policy` 和 `--sandbox-mode` 向现有 owner 解码器和权限检查提交
提议。对应的 `--set` 键为 `provider`、`model`、`default_text_model`、
`approval_policy` 和 `sandbox_mode`。凭据和端点由 owner 保管：这些控制拒绝
`--api-key`、`--base-url` 和其他单次运行设置。请先配置并认证所属 Runtime。
携带保留的 `--operation-key` 时，新提交的模型、提供商、策略或 sandbox 提议
都会被拒绝；恢复只观察原本已接纳的意图。

交互式 `codewhale resume` 和 `codewhale fork` 使用同一个规范历史操作。
只有在非活动的本地 owner 已关闭并等待退出之后，现有 TUI 才取得会话租约和存储。
活动 owner 或不确定的交接会拒绝挂接。`--operation-key <KEY>` 恢复原始结果；
可以完成已验证并准备好的目标；尚未准备好或无法验证的结果保留不确定性。

### 持久配置写入

`app/config/set` 与 `app/config/unset` 在回复前写入规范 owner 使用的配置文件：
显式 `--config` 路径，或默认 `config.toml`。设置在 app-server 退出后仍保留。
变更在锁内重新读取并合并磁盘当前文件，保留其他进程已保存的编辑。
文件无法读取、解析或写入时返回 `ok: false`，不会更改它；解析失败的文件不被重写，
请先手工修好再 `app/config/reload`。HTTP `/app` 的非法键/值返回 400，I/O 失败返回 500。

### 回答澄清提问

当一个无头回合调用 `request_user_input` 时，运行时会发出一个
`user_input.required` 事件，携带 `request_id`。请通过运行时 API 回复：

```
POST /v1/user-input/{thread_id}/{request_id}
```

兼容控制传输无法接受该回复。`app/request` 的 `SubmitUserInput` 返回
`ok: false` 与 `error: "user_input_reply_unsupported"`。兼容回合要求空工具列表，
交互执行在准入时拒绝，因此受支持的回合不会静默等待无法提交的答案。
这是 Codewhale 当前实现的限制。

## SDK 契约

app-server 存在的意义是让外部 SDK 无需抓取 TUI
输出就能回答——*实际运行了哪条路由、生效的提供商/模型/推理/权限配置是什么、
发生了哪些事件、用了多少 token、这次运行如何结束。* 持久化的 Thread/Turn/Item 数据模型
已经承载了其中大部分内容；下表把每一项集成需求映射到本地客户端读取它的位置。

| 集成需求 | 来源 | 状态 |
|---|---|---|
| 路由 / 生效模型 / 计费表面 | `TurnRecord` + 线程 `model`；每次运行的 `--provider`/`--model` 覆盖 | 可用 |
| 权限 / 沙箱 / 审批配置 | 线程 `auto_approve`、沙箱 + 审批策略；`TurnRecord.permission_posture` + `TurnRecord.mode` 说明*那一次*运行是如何被治理的（该线程自己的 `mode` 此后可能已被切换） | 可用 |
| 运行 / 线程 / 回合 ID | `thread_id`、`turn_id`、SSE 事件信封 | 可用 |
| 事件流 | `GET /v1/threads/{id}/events`（回放 + 实时 SSE） | 可用 |
| 回合状态 / 终态分类 | `TurnRecord.status` + 错误摘要 | 可用 |
| Token 用量 | `TurnRecord.usage`；通过 `GET /v1/usage` 聚合 | 可用 |
| 动作回执（文件、命令、web/MCP 调用、智能体、审批及由谁决定、失败） | `GET /v1/threads/{id}/receipt`、`GET /v1/threads/{id}/turns/{turn_id}/receipt` | 可用（[RECEIPTS.md](../RECEIPTS.md)） |

对于一次性/无头自动化，优先使用 `codewhale exec` 并显式给出
`--provider <id> --model <id>`，这样一旦失败就能确定是哪一对提供商/模型。
当本地集成需要启动、恢复、引导（steer）或中断回合、列出模型/能力、跟踪事件流
或读取用量时，请使用 `app-server`。两条路径共享同一个运行时，因此路由生效的模型解析
与事件词汇表是一致的。

### 发布冒烟检查

`scripts/release/app-server-smoke.sh` 是已提交的发布前检查：

```bash
scripts/release/app-server-smoke.sh                 # stdio health/capabilities probe (no tokens)
scripts/release/app-server-smoke.sh --matrix        # + print the configured provider/model matrix
scripts/release/app-server-smoke.sh --matrix --real # + exec a cheap sentinel per provider
```

stdio 探针针对一份一次性配置运行，因此它从不读取真实密钥。
矩阵从 `codewhale auth list` 发现已配置的提供商，跳过
未配置的提供商，并且只有当某个提供商有内置廉价默认模型时才把它映射到一个
廉价哨兵模型。这个内置集合是刻意保守的
（目前是 `deepseek`、`zai`、`moonshot` 与 `openai`）；其他每个提供商——
包括 `arcee`、`openrouter`、`xiaomi-mimo` 与 `openai-codex`——都被有意留作未映射，
每次运行必须通过 `SMOKE_MODEL_<SLUG>` 指定模型，而不是使用猜测的默认值（#3205）。
任何已配置但未映射的提供商在 `--real` 模式下都会大声失败。`auth list` 只报告存在性标志，
且 exec 输出会经过脱敏器，因此密钥永不会被打印。该解析器由
`scripts/release/app-server-smoke.test.sh` 针对一个伪造的 `codewhale`
二进制进行覆盖。

## ACP stdio 适配器：`codewhale serve --acp`

ACP 以换行分隔的 stdio JSON-RPC 投影现有 RuntimeThreadManager 与 Engine。
它不再维护独立的提供商/工具回合循环、可执行注册表、提示词组合器或会话写入器。
服务端加载实际选定的配置、profile 与插件发现结果；每个提示词复用规范线程、
Core 回合、事件时间线、审批等待器和完整 Engine 会话快照。

编辑器接口支持 `initialize`、`session/new`、`session/list`、`session/load`
（含持久 ID 前缀）、`session/prompt`、`session/cancel`、模型发现/选择，
以及声明的模式/模型配置选项。新会话先持久化裸 UUID 和空检查点，不调用提供商。
`session/load` 在响应前以 `session/update` 通知重放已保存的对话：用户与智能体
文本，以及每个工具调用及其最终状态。重放的工具结果与实时流一样脱敏：
`request_user_input` 的回答显示为 `User input submitted`，已配置的凭据被遮蔽。
推理内容和图片不会重放。
连接最多保留64个空闲绑定；淘汰绑定不删除持久会话。恢复复用已有线程绑定。
完整历史、工具调用/结果配对、签名、媒体和部分执行回执通过 HTTP 同用的检查点
守卫与会话写入租约保存。ACP 展示文本可以缩短，持久历史仍是完整 Core 快照。

Codewhale 0.10.2 的 ACP v1 支持不完整。`session/new` 和 `session/load`
会在创建、恢复或返回缓存会话之前拒绝非空 `mcpServers`，返回 `-32602`。
该字段可省略或为 `[]`；null 与其他非数组值无效。验证不会执行所提供的命令。
[ACP v1 要求 stdio MCP 支持](https://agentclientprotocol.com/protocol/v1/session-setup#mcp-servers)；HTTP/SSE 能力为 false 并不代表完整合规。
需要 MCP 的集成请用完整 Runtime API。下列工具限制来自 Codewhale host profile，
不是 ACP 协议本身的限制。

受信任的本地 ACP profile 将 Core 收窄到文件/搜索/git/patch，以及获准的前台
shell 工具。Shell 同时要求编辑器声明 terminal 支持、操作者允许 `allow_shell`；
指定的外部沙箱不可用时不提供 shell。此接口不提供 MCP、动态工具、任务、PTY、
后台 shell、解释器、子智能体或 RLM 生命周期。内置工具覆盖会移除整个兼容别名族。
最终派发再次校验 profile；伪造别名、hook 改写或目录中缺失的工具不能绕过限制。
Full Access 下 Plan 仍只读。Full Access 与普通审批姿态是服务端拥有的只读选项，
编辑器不能放宽。`approval_policy = "never"` 不是 ACP 审批姿态，服务端会拒绝
以该配置启动，而不是把它当作 Ask 提供。Core 的类型化规则、严格 hooks、仓库约束、Headless Auto-Review
与硬性下限仍生效；工作区写入不享受免审批例外，需要 guardian 的裁决会明确拒绝。

工具首先显示 `pending`；只有 Core 到达最终派发才显示 `in_progress`。
`completed`/`failed` 和类型化图片块来自实际 Core 结果。审批请求的私有 JSON-RPC ID
绑定同一个 Runtime 铸造的待决审批与 Core 执行 ID。只有精确匹配、仍有效的
`allow-once` 响应能释放等待器；错误 ID 被忽略，无效选项拒绝，取消会撤销等待器。
ACP 不授予记忆权限或 Native 能力。

重放复用有界事件读取器，按序号去重，并在每个事件之间处理输入；每次传输写入
最多等待30秒。重放缺口、
owner 关闭或无法产生终态的存储故障会明确报错，不会重新执行。取消、EOF 和写入器
故障只中断本连接实际声明的 Core 回合；结算依赖真实终态回执并保留已完成的效果。
无法确认取消时不伪造成功。`stopReason` 为 `end_turn`、`cancelled` 或类型化的
`max_turn_requests`；Core 失败仍是错误。单次提示词最多50个模型步骤（更小的配置
上限仍有效），并复用 Core 的有界最终报告响应；此 profile 不派发自主目标续跑。

没有进程持有 Runtime 存储时，ACP 声明自己独占的规范 Runtime owner。已有非 ACP
的 Runtime 宿主持有该存储时（桌面应用、`codewhale app-server --http` 或
`codewhale serve --http`），ACP 通过其本地控制套接字以经过身份校验的访客身份
附着到该 owner，并用 owner 的存储服务编辑器。该 owner 退出时，ACP 进程以说明
该退出的错误结束；待决操作的结果可能不确定，且不会重放。存储已由另一个 ACP
服务端持有时，第二个 `codewhale serve --acp` 在启动时拒绝，第一个继续服务。
会话绑定另一 Runtime 存储时也拒绝；ACP 不把活跃会话复制到随机存储。它也不向
编辑器提供全部 `/v1/*` 引导、任务或控制方法；完整运行时 API 请使用
`codewhale app-server --http`。


## 能力端点：`codewhale doctor --json`

返回一个 JSON 对象，描述当前安装的就绪状态。
适合 macOS 工作台做健康检查轮询。该命令严格是结构性且离线的：它不加载工作区凭据
`.env` 文件、不检查凭据环境变量值、不打开 secret/OAuth 文件、
不探测 OS 钥匙串、不联系提供商、也不启动 MCP 进程。

```bash
codewhale doctor --json
```

### 响应 schema（关键字段）

| Field | Type | Description |
|---|---|---|
| `version` | string | 已安装的版本（例如 `"0.8.9"`） |
| `config_path` | string | 解析后的配置文件路径 |
| `config_present` | bool | 配置文件是否存在 |
| `paths` | object | 规范的配置、设置、状态、会话、日志、自动化与 secrets 路径 |
| `secret_backend` | object | 仅含元数据的文件存储形态；对系统后端与不支持的后端则为字面量 `unknown` / `not_probed` |
| `workspace` | string | 默认工作区目录 |
| `legacy_state.primary_root` | string | 为主状态路径被检查的 Codewhale 主状态根 |
| `legacy_state.legacy_root` | string | 为已知状态路径被检查的旧式 `.deepseek` 状态根 |
| `legacy_state.needs_attention` | bool | 已知的 `~/.deepseek` 状态路径是否需要人工核查，或只读会话恢复诊断发现目标文件名缺失 / 无法完成 |
| `legacy_state.legacy_only_count` | number | 仅存在于旧式根下的已知状态路径数 |
| `legacy_state.dual_present_count` | number | 同时存在于主根与旧式根下的已知状态路径数 |
| `legacy_state.entries` | array | 逐路径的迁移状态：`{name, primary_present, legacy_present, status}` |
| `legacy_state.session_recovery.status` | string | `isolated`、`no_legacy_sessions`、`migration_pending`、`migration_incomplete`、`migration_complete` 或 `scan_failed` |
| `legacy_state.session_recovery.read_only` | bool | 恒为 true；doctor 永不触发会话迁移，也不修改任一会话目录 |
| `legacy_state.session_recovery.chat_contents_read` | bool | 恒为 false；比较仅基于顶层 `.json` 文件名与文件系统元数据 |
| `legacy_state.session_recovery.checkpoint_internals_scanned` | bool | 恒为 false；`sessions/checkpoints/` 及其他所有目录均被跳过 |
| `legacy_state.session_recovery.recoverable_files` | array | 最多 100 个缺失目标文件名的有界样本，带来源与目标路径；不含对话负载 |
| `legacy_state.session_recovery.recoverable_file_count` | number | 缺失目标文件名的总数，含超出有界样本的条目 |
| `legacy_state.session_recovery.recoverable_files_truncated` | bool | 是否发现了多于 100 个可恢复文件名 |
| `legacy_state.session_recovery.recovery_command` | string or null | 当可加性自动恢复可用时为 `codewhale sessions`；隔离、已完整、为空或扫描失败时为 null |
| `api_key.source` | string | 结构性的来源状态：`config_declared`、`env_declared`、`external_auth_declared`、`secret_store_unprobed`、`secret_store_unavailable`、`oauth_unprobed`、`external_consent`、`none`、`local_runtime` 或 `unknown`；声明不等于可用性证明 |
| `api_key.availability` | string | 字面量 `present`、`not_required`、`not_probed`、`unavailable` 或 `unknown`；只有 `present` 与 `not_required` 能证明 Setup/Fleet 凭据在结构上已就绪 |
| `base_url` | string | 仅提供商 URL 的授权部分（`scheme://host[:explicit-port]`）；userinfo、path、query 与 fragment 均被省略 |
| `default_text_model` | string | 默认模型 |
| `memory.enabled` | bool | 记忆功能是否开启 |
| `memory.path` | string | 记忆文件路径 |
| `memory.file_present` | bool | 记忆文件是否存在 |
| `mcp.config_path` | string | MCP 配置文件路径 |
| `mcp.present` | bool | MCP 配置是否存在 |
| `mcp.probe_scope` | string | `configuration`；doctor 不启动 MCP 服务器 |
| `mcp.live_health_checked` | bool | 对 doctor JSON 恒为 false |
| `mcp.servers` | array | 逐服务器的结构性结果与计数，另加单独的 `checks`；URL 的 userinfo/path/query/fragment 以及命令 argv、环境、header 与 token 值永不输出，且所有实时阶段均为 `not_checked` |
| `skills.selected` | string | 解析后的技能目录 |
| `skills.global.path` / `.present` / `.count` | — | Codewhale 全局技能目录（`~/.codewhale/skills`，并支持旧式 `~/.deepseek/skills`） |
| `skills.agents.path` / `.present` / `.count` | — | 工作区 `.agents/skills/` 目录 |
| `skills.agents_global.path` / `.present` / `.count` | — | agentskills.io 全局技能目录（`~/.agents/skills`） |
| `skills.local.path` / `.present` / `.count` | — | `skills/` 目录 |
| `skills.opencode.path` / `.present` / `.count` | — | `.opencode/skills/` 目录 |
| `skills.claude.path` / `.present` / `.count` | — | `.claude/skills/` 目录 |
| `tools.path` / `.present` / `.count` | — | 全局工具目录 |
| `plugins.path` / `.present` / `.count` | — | 全局插件目录 |
| `sandbox.available` | bool | 该操作系统上是否支持沙箱 |
| `sandbox.kind` | string or null | 沙箱种类（例如 `"macos_seatbelt"`） |
| `storage.spillover.path` / `.present` / `.count` | — | 工具输出溢出目录 |
| `storage.stash.path` / `.present` / `.count` | — | 输入区暂存区 |

### 示例

```json
{
  "version": "0.8.9",
  "config_path": "/Users/you/.codewhale/config.toml",
  "config_present": true,
  "workspace": "/Users/you/projects/codewhale-tui",
  "api_key": {
    "source": "secret_store_unprobed",
    "availability": "not_probed"
  },
  "base_url": "https://api.deepseek.com",
  "default_text_model": "deepseek-v4-pro",
  "memory": {
    "enabled": false,
    "path": "/Users/you/.codewhale/memory.md",
    "file_present": true
  },
  "mcp": {
    "config_path": "/Users/you/.codewhale/mcp.json",
    "present": true,
    "servers": [
      {"name": "filesystem", "enabled": true, "transport": "stdio", "args_count": 2, "env_count": 0, "status": "ok"}
    ]
  },
  "sandbox": {
    "available": true,
    "kind": "macos_seatbelt"
  }
}
```

## HTTP/SSE 运行时 API：`codewhale app-server --http`

```bash
codewhale app-server --http [--host 127.0.0.1] [--port 7878] [--workers 2] [--auth-token TOKEN] [--insecure-no-auth]
codewhale app-server --mobile [--host 127.0.0.1] [--port 7878] [--auth-token TOKEN]
codewhale app-server --mobile --host ::1 [--port 7878] [--insecure-no-auth]
codewhale web [--port 7878]

# Compatibility aliases — identical server, serve flag names:
codewhale serve --http   [...] [--insecure]
codewhale serve --mobile [...] [--insecure]
```

默认值：主机 `127.0.0.1`、端口 `7878`、2 个 worker（限制在 1–8）。

服务器默认绑定到 `localhost`。配置通过 CLI 标志完成——
没有 `[app_server]` 配置节。

`/v1/*` 路由需要 bearer 令牌，除非 `codewhale app-server` 在诸如 `127.0.0.1` 的
回环绑定上以 `--insecure-no-auth` 启动。移动模式
仅限回环：在 Runtime 拥有 TLS 或经过验证的 overlay 传输边界之前，
非回环主机都会被拒绝。`codewhale serve` 兼容别名
用 `--insecure` 作为同一个回环逃生通道。
启动服务器前请传入 `--auth-token TOKEN` 或设置 `CODEWHALE_RUNTIME_TOKEN=TOKEN`；
`DEEPSEEK_RUNTIME_TOKEN` 仍作为兼容别名保留。两者都未设置时，
进程会为该进程生成一个 Runtime 令牌，且**不会**
打印它。`/health`、`/v1/runtime/info` 与已启用的静态客户端外壳
保持公开；Runtime 的变更操作与线程数据留在 `/v1/*`
认证之后。移动模式被禁用时 `/mobile` 返回 404，启用时
提供未改动的静态外壳。

已认证的客户端可以提供令牌：`Authorization: Bearer TOKEN`、
`X-Codewhale-Runtime-Token: TOKEN`，或旧式的
`X-DeepSeek-Runtime-Token: TOKEN`。查询字符串认证与裸 Runtime 令牌 cookie
认证不受支持。

### 本地浏览器客户端

`codewhale web` 在 `127.0.0.1` 上启动规范 Runtime API，提供
嵌入二进制的无依赖资源，打印一个一次性启动 URL，
并要求操作系统在默认浏览器中打开该 URL。如果
浏览器没有打开，打印出的 URL 在十分钟内仍然可用。该
命令不能绑定非回环主机，也不能在 Runtime 认证
被禁用的情况下运行。

浏览器启动 URL 包含一个随机的、短期有效的一次性 bootstrap
能力，绝不是 Runtime 令牌。一个回环请求会用该
能力换取一个
`codewhale_web_session=…; HttpOnly; SameSite=Strict; Path=/` cookie，其背后是
单个进程内服务器会话：它在服务器进程启动 12 小时后过期，
立即消耗该能力，并重定向到 `/`。重复使用、已过期、格式错误或
非回环的 bootstrap 尝试都会失败关闭。Runtime bearer 令牌不会被
写入渲染的 HTML、浏览器存储、日志、URL 查询/fragment 或
浏览器启动参数。一次性 bootstrap 能力会打印在
本地终端，并经由操作系统浏览器启动器的参数列表传递。同用户
进程可能抢在浏览器之前完成交换，这正是该能力
只能用一次、仅限回环并在十分钟后过期的原因——也是为什么同用户
攻击者有严格比这个竞争更简单的本地途径。
Web 请求需要会话 cookie 加上一个限定于源（origin）的请求证明；
流使用一张新的一次性票据。初始重定向把证明放在 fragment 中，客户端将其移除并
保存到限定于源的 `sessionStorage`；只要浏览器允许会话存储，重新加载该标签页
仍可继续使用。服务器从不把证明写进页面：会话 cookie 只限定主机而不限定端口，
另一个回环端口上的监听者可能收到它，因此仅凭 cookie 不能取回证明。通过手动输入
地址打开的新标签页没有证明；请使用原来的标签页、复制该标签页，或重新启动
`codewhale web` 获取新的一次性会话。该页面使用 `no-store`、禁止被嵌入框架，
也不授予任何跨源读取权限。任何操作都不会延长服务器会话，也不会替换已过期的 cookie。
跨源的 Fetch Metadata 或不匹配的 Origin 会在 web API 请求上被拒绝。显式的
bearer 与 Runtime 令牌 header 客户端保持其既有行为。暂时性的流票据失败会以
有上限的退避重试；HTTP 401/403 会停止票据重试，直到打开新的会话。

内嵌客户端提供一个响应式线程/搜索侧栏、Runtime 拥有的
会话事实、转录与工具回执，以及底部输入区。它可以
创建、选择、重命名与归档线程；为新线程选择提供商与模型
而不改变 Runtime 默认值；启动或引导回合；中断
工作；解决审批；并回答 Runtime 的用户输入请求。选择某个线程时
会先加载 `GET /v1/threads/{id}`，然后用
`since_seq=latest_seq` 打开可回放的事件流；重连时从最新被接受的序号
继续前进，并丢弃重复事件或来自过期选择的事件。线程详情
快照包含 `pending_approvals`、`pending_user_inputs` 与
`pending_dynamic_tool_calls`；客户端必须在
订阅之前先填充这些字段，这样一次重新加载就不会把请求事件位于或早于
`latest_seq` 的工作搁置在那里。对已连接的客户端，解决结果也会以 `approval.decided`、
`user_input.answered`、`user_input.canceled`、`tool_call.resolved`、
`tool_call.canceled` 或 `tool_call.timeout` 发布。

既有线程的模型、模式、权限姿态、工作区与分支在该客户端中
仅作展示。Files/Changes、PTY/终端、预览、工件、
提供商登录或全局默认值切换、Fleet 创建，以及
撤销/重试/恢复控件都没有内置在这个页面里。原生桌面
客户端通过本文记录的工作区文件、回合工件、终端与工作区恢复路由提供它们。

### 移动端控制页

`codewhale serve --mobile` 启动同一个 HTTP/SSE 运行时 API，并在 `/mobile` 提供一个
适配手机的控制页。它只绑定回环
（`127.0.0.1` 或 `::1`）；非回环主机会被拒绝，因为该 Runtime
表面尚未提供 TLS 或经过验证的 overlay 传输。静态
HTML 页面不含 Runtime bearer，本身也不受令牌门控。当
Runtime 认证启用时，CLI 会打印一个短期有效的、一次性回环
bootstrap URL。该能力会创建一个 30 分钟的进程内
`Max-Age=1800; HttpOnly; SameSite=Strict` 移动端会话 cookie，以及按源站限定范围的浏览器
证明。一个收到该主机范围 cookie 的兄弟端口无法仅凭它使用它。
该页面也可以一次性交换显式输入的 bearer，随后
清除它，而不是把它存进浏览器存储或 cookie。EventSource
连接使用单独的短期有效、一次性流票据。

移动端页面可以列出/创建线程、发送提示词、跟踪实时 SSE 事件、
引导或中断活动回合，并通过
`POST /v1/approvals/{approval_id}` 解决常规工具审批。它是一个仅限本地的便捷表面；
在 Runtime 拥有 TLS 或经过验证的传输边界之前，
不要直接将它暴露给其他设备或公网。

### 端点

**健康检查**
- `GET /health`

**会话**（持久会话管理器）
- `GET /v1/sessions?limit=50&search=<fuzzy>&include_archived=false&archived_only=false&workspace=<path>&sort=recent|name|size`
- `GET /v1/sessions/summary?…`（相同的查询参数；投影后的行形态）
- `GET /v1/sessions/{id}`（加上 `?peek=true&entries=12` 可得到有界的、已脱敏的
  只读窥视，而不是完整转录）。完整响应会在某个回合以 `Failed` 结束时携带
  `turn_outcomes`：每次失败一条 `{ status, error, ended_at,
  after_message_count }`，最旧的在前，最多 64 条，错误文本与转录中显示的一致，
  且已对密钥脱敏
- `PATCH /v1/sessions/{id}`（`{ "title"?: string, "archived"?: bool }`）
- `DELETE /v1/sessions/{id}`
- `POST /v1/sessions/{id}/resume-thread` 返回已经持有整个已保存会话的打开线程
  （`200`）；没有这样的线程时，从该会话播种一个新线程（`201`），包括会话在那个
  线程打开它之后又有增长的情况。请求体未给出 `mode` 时，新线程沿用
  已保存的模式；不是 Runtime 模式的已保存模式（例如 `exec`）以 `agent` 恢复。
  新线程保存的工具结果与实时回执一样脱敏：`request_user_input` 的回答记为
  `User input submitted`，已配置的凭据被遮蔽。`GET /v1/sessions/{id}` 仍返回未脱敏的内容。
- `GET /v1/sessions/{id}/artifacts` 与 `GET /v1/sessions/{id}/artifacts/{artifact_id}?offset=&limit=`
  （参见上文的工作区文件与会话工件；运行时回合的溢出通过
  `GET /v1/threads/{id}/turns/{turn_id}/artifacts/{artifact_id}` 读取）
- `POST /v1/sessions`（`{ "thread_id": string, "title"?: string }`）把线程导出为
  已保存会话。它是幂等的：只写入 id 由该线程派生的那份文档，第一次创建它（`201`），
  之后更新它（`200`），所以重试永远不会产生重复。线程从中恢复而来的会话保持不变。
- `PUT /v1/sessions`（`{ "thread_id"?: string, "session_id"?: string }`）保存线程的
  实时对话。指名一个已绑定到另一个线程的 `session_id` 会返回 `409 Conflict`。
- `GET /v1/sessions/repair` 返回最近一次会话存储修复的摘要；从未运行过时为 `null`

会话与线程对同一对 `include_archived` / `archived_only`
给出含义相同的响应，并且 `search` 与 TUI 会话选择器及任务面板（workbar）
中的 Sessions 列表使用的是同一个模糊匹配（标题、id、
工作区——先子串，再子序列）。三个表面运行同一套投影
（`crates/engine/src/session_projection.rs`），因此列表在
终端与仪表盘之间不可能有差异。

`GET /v1/sessions/summary` 返回的行与
`GET /v1/threads/summary` 字段兼容——`id`、`title`、`preview`、`model`、`mode`、
`workspace`、`archived`、`updated_at`——另加 `message_count`、`total_tokens`、
`created_at`、`parent_session_id` 与 `is_current`。有一个需要直说的注意点：
`preview` 是该会话记录的**标题**，不是它的最后一条消息。会话
元数据不存储最后一条消息，而为了合成一条而读取每个转录
会让列表视图变成无界读取。完整转录
预览位于 TUI 会话选择器中，它只读取所选的那一个会话。

`PATCH /v1/sessions/{id}` 重命名并/或归档一个已保存会话，并返回一个
形态与线程 patch 回执相同的生命周期回执：

```json
{
  "session": { "id": "…", "title": "Renamed", "archived": true, "…": "…" },
  "changes": { "title": "Renamed", "archived": true }
}
```

`changes` 只列出实际发生变动的部分，因此空操作（no-op）patch 与
已生效的 patch 可以被区分开来。归档是持久且可逆的：已归档会话
仍保留在磁盘上且仍可加载，只是从默认列表中消失，并且
永不会被 `--continue` 或自动恢复选中。该路由与 TUI 选择器（`e`）及
`/sessions archive <id>` 用的是同一个写入方——不存在第二套
归档概念。

当一个会话在某个交互式 Codewhale 进程中处于打开状态时，该进程持有
内存中的权威副本，并在下一次自动保存时重写整个文档。因此对它的 `PATCH`、`PUT`
与 `DELETE` 会失败关闭并返回 `409 Conflict`，而不是写入一个会被静默回滚的内容。
请在终端中修改它。打开它的进程持有该会话的锁（`sessions/.late-usage/<id>.live`），
所以无论请求到达的是该进程内部的 API，还是另一个独立的 `codewhale serve`，这一点都成立。

会话存储会在每次启动和每次 `codewhale serve` 启动时在后台修复。修复会为 Runtime
存储中每个没有绑定任何会话的线程分配一个“Recovered:”会话。它会解除那些会话文档
已消失的线程的绑定；这些线程随后从它们自己的回合加载。它会把不可读的文档、空的
未绑定存储，以及没有任何会话引用的旧工件目录移到 `sessions/.set-aside/<run>/`，
并在那里写入一份 `MANIFEST.jsonl`。不会删除任何东西。`GET /v1/sessions/repair`
与 `codewhale doctor` 报告最近一次运行；`codewhale doctor --repair-sessions [--dry-run]`
按需运行一次。

`GET /v1/sessions/{id}?peek=true` 返回一个有界的、已脱敏的、只读的视图，
而不是转录本身：最多 12 个条目、每个最多 400 个字符
（`&entries=N` 只会调低预算，绝不会把它抬过上限），工具调用与
结果被概括为一个名称和一个大小而不是内联展开，凭据形态的
子串会被掩码。`omitted_before` 报告有多少更早的消息被丢弃。
负载携带 `"live": false`，并有意不包含回合状态、`running` 或 `active` 字段——已保存的会话是一份录像，
实时状态只来自已恢复线程的 SSE 流。

**线程**（持久运行时数据模型）
- `GET /v1/threads?limit=50&include_archived=false&archived_only=false`
- `GET /v1/threads/summary?limit=50&search=<optional>&include_archived=false&archived_only=false&thread_ids=<id>,<id>`
- `GET /v1/threads/running`
- `GET /v1/threads/{id}/notices`
- `DELETE /v1/threads/{id}/notices/{notice_id}`
- `POST /v1/threads`
- `GET /v1/threads/{id}`
- `PATCH /v1/threads/{id}`（请求体形态见下文）
- `POST /v1/threads/{id}/resume`
- `POST /v1/threads/{id}/fork`
- `POST /v1/threads/{id}/read` — 记录客户端已显示到的最新条目
  （请求体 `{"item_id": "<item id>"}`；见下文“已读标记”）
- `GET /v1/threads/{id}/receipt` — 线程做了什么，每个动作一条
  （只读；形态见 [RECEIPTS.md](../RECEIPTS.md)）
- `GET /v1/threads/{id}/turns/{turn_id}/receipt` — 同上，针对一个回合；
  未知线程或不属于该线程的回合返回 `404`

`thread_ids` 是逗号分隔的列表，最多 200 个 ID（每个 ID 最多 128 字节）。路由会在应用
`limit` 前按精确 ID 过滤，并保留最新优先顺序。这只是选择过滤器，不是
所有权或授权检查；Runtime bearer token 仍是访问边界。

`POST /v1/threads` 除了提供商、模型、工作区与权限字段外，还接受可选的执行默认值：

```json
{
  "model_provider": "openai-codex",
  "model": "gpt-5.6",
  "reasoning_effort": "high",
  "allowed_tools": ["read_file", "search"]
}
```

`reasoning_effort` 使用规范的 Runtime 词汇表（`auto`、`off`、
`low`、`medium`、`high`、`xhigh`、`ultra` 或 `max`；已记录的兼容
别名会被接受，并以规范形式持久化）。`allowed_tools` 是一份对模型可见的允许名单。
省略它会保留常规的已配置目录；显式空数组（`"allowed_tools": []`）
则不会向模型暴露任何工具。
两个字段都是加性（additive）的：省略它们的旧线程记录与客户端
保持之前的行为。

`GET /v1/threads/summary` 是 VS Code Agent View 使用的只读摘要表面。
`search` 匹配线程的 `id`、`title` 与 `model`（当标题未设置时，还会匹配最近一个回合的
输入摘要——即被展示的标题）。它不扫描回合或条目正文：`preview` 只在
命中之后才被填充，因此仪表盘上一次按键不是逐线程的全量存储读取。每个条目包含
`id`、`title`、`preview`、`model`、`mode`、`archived`、`updated_at`、
`latest_turn_id`、`latest_turn_status`，再加工作区元数据：

```json
{
  "id": "thread_...",
  "title": "Implement MCP status count",
  "preview": "The TUI footer should count project MCP servers...",
  "model": "deepseek-v4-pro",
  "mode": "agent",
  "branch": "feature/runtime-api",
  "head": "abc1234",
  "dirty": false,
  "workspace": "/Users/you/projects/codewhale",
  "archived": false,
  "updated_at": "2026-06-06T05:43:00Z",
  "latest_turn_id": "turn_...",
  "latest_turn_status": "completed"
}
```

每行还带有来自线程已读标记的 `last_read_item_id` 与 `unread`（见下文
“已读标记”）。从未被任何客户端标记为已读的线程，两者都为 `null`。

`branch` 在请求时从线程工作区解析得到，当工作区不是 Git 仓库或
分支无法读取时可能为 `null`。
`head` 是该工作区当前可得的短 Git commit。
`dirty` 在工作区有已暂存、未暂存或未跟踪的改动时为 true。
包含 `workspace` 是为了让编辑器客户端能显示某个智能体通道何时在
当前 VS Code 文件夹之外工作。

线程 fork 是兄弟运行时线程，不是就地（in-place）的树投影。
`thread.forked` 事件包含 `source_thread_id`；内部的回溯感知
fork 还可能包含 `backtrack_depth_from_tail` 与 `dropped_turn_id`，而
锚定到具名回合的 fork（`/fork-at-turn`）会报告它们，其深度
从该回合解析得出，并点名它丢弃的第一个用户回合（不是锚点，具名回合 fork 会保留锚点，
也不是夹在两者之间的无提示词回合，比如一次手动压缩）。
在 v0.8.40 中，线程列表与摘要响应仍是扁平的，因此需要
图的客户端应当从事件重建它，而不是假定列表顺序就是
一棵完整的树。

### 已读标记

`POST /v1/threads/{id}/read` 携带 `{"item_id": "<item id>"}`，记录客户端已把对话
显示到该条目为止，顺序与 `GET /v1/threads/{id}` 返回条目的顺序一致（先按回合，再按
条目开始时间）。客户端在用户打开或阅读对话时，发送它渲染出的最新条目的 id。该条目
必须属于此线程：未知线程返回 `404`，未知条目或属于其他线程的条目返回 `400`。响应是
线程记录，其中 `last_read` 包含 `item_id`、`last_in_turn`（标记时该条目是否为其所在
回合的最后一个条目）与 `watermark_item_id`（那一刻该回合已记录的最后一个条目）。

标记已读不会改动其他任何东西：`updated_at`、列表顺序和线程事件流都保持不变，所以打开
对话永远不会把它移到列表顶部，也不会让它重新变成未读。标记只会向前移动：标记一个位于
当前标记之前的条目时，标记保持不变，因此显示旧快照的窗口无法把已读对话变回未读。fork
出的线程不带标记。

`GET /v1/threads/summary` 根据标记推导 `unread`：被标记的条目仍是最新条目时为
`false`，一旦出现任何更新的条目即为 `true`，线程从未被标记时为 `null`（旧版 Runtime
没有标记，因此升级后不会把所有已有对话都报告为未读；对这些线程，客户端保留自己的
回退逻辑）。“新消息”分隔线应放在 `last_read_item_id` 所指条目之后。已知限制：
`unread` 跟随每个回合在运行时记录的条目，所以一个已开始但尚未记录任何条目的回合，在
记录条目之前不计入；回合结束后才添加到该回合的通知（例如权限变更）也不计入。

`GET /v1/threads/running` 是进行中工作的核算表面
（#6180）：至少有一个排队或进行中回合的线程，每个都带有
`thread_id`、`model`、`title` 与 `active_turns`（`turn_id` + `status`）。
具备后台能力的客户端用它做退出/转后台决策——一次调用，
不需要从最近回合状态去推断。归档状态被忽略（归档
没有静默门槛）；空数组意味着没有自己拥有的活动工作。

`GET /v1/threads/{id}/notices` 是逐线程的活动通知表面
（#6180）：TUI 可见、且只读客户端必须呈现的那些状况——
`subagent-terminal`（一个子智能体已完结）、`elevation-needed`（一个工具调用
被提权拦住）、`model-notify`（模型请用户回来）——各自带有 `turn_id` 与一个用于定位的 `subject` id。
通知是内存中的会话状态，每个线程最多 32 条（最旧的被淘汰），
且永不持久化。清除：提权在它的工具调用
完成时自动清除；terminal/notify 通过 `DELETE .../notices/{notice_id}` 清除
（204，未知 id 返回 404）。未知线程在两个端点上都是 404。

`archived_only=true` 只返回已归档线程（互斥地覆盖
`include_archived`）。默认行为不变：`include_archived=false`
与 `archived_only=false` 返回活动线程。于 v0.8.10 加入（#563）。

`PATCH /v1/threads/{id}` 请求体——每个字段都是可选的，缺失
表示“不改变”。至少必须有一个字段存在。`title` 与 `system_prompt`
接受空字符串，用于清除先前设置的值。于 v0.8.10 加入（#562）：

```json
{
  "archived": true,
  "allow_shell": false,
  "trust_mode": false,
  "auto_approve": false,
  "model": "deepseek-v4-pro",
  "mode": "agent",
  "title": "User-set thread title",
  "system_prompt": "You are a useful assistant.",
  "model_provider": "custom",
  "model_provider_id": "lm-studio"
}
```

`system_prompt` 的修改从下一个回合开始生效：已加载的引擎会被卸载，并用新的提示词根据线程历史重建。与修改 `workspace` 一样，线程有正在运行的回合时会拒绝该修改。

`model_provider` 切换该线程未来回合所用的提供商。它
接受内置种类（`deepseek`、`xai`、...）或已配置的路由名，与
`/provider` 一样。`model_provider_id` 指名一个精确的 `[providers.<id>]` 表，
并优先于路由名。目标路由会被解析，其客户端会在保存任何内容之前
被预检，因此未知或没有凭据的
提供商会遭拒绝，且什么都不会改变。若不带 `model`，线程会采用
新提供商的默认模型；`auto` 线程保持 `auto`。已加载的
引擎与会话历史被保留，下一个回合会装上新的
路由。

有效权限发生变化时，会在线程最新回合追加一个已完成的 `status` 条目，并发布
`thread.permissions_changed` 事件（`from`、`to`、`source`）。

**回合**（线程内的）
- `POST /v1/threads/{id}/turns`
- `POST /v1/threads/{id}/turns/{turn_id}/steer` - 向进行中的回合注入引导。响应是一份描述实际发生了什么的回执，而不是描述尝试过什么的回执；参见 [引导送达](#引导送达)。
- `POST /v1/threads/{id}/turns/{turn_id}/interrupt`
- `GET /v1/threads/{id}/turns/{turn_id}/artifacts` - 回合产出了什么：类型化引用加上工作区 delta 状态。参见 [回合工件](#回合工件)。
- `GET /v1/threads/{id}/turns/{turn_id}/artifacts/{artifact_id}?offset=&limit=&revision=` - 从工作区、post-turn 快照或会话工件目录读取一个引用。
- `POST /v1/threads/{id}/compact`（手动压缩）
- `POST /v1/threads/{id}/undo` - 以去掉最后 N 个回合的方式 fork 线程（`{"depth": N}`，默认 0 = 仅最后一个回合）；返回 fork 出的线程以及 `original_user_text`，以便 GUI 预填输入框。改用 `{"turn_id": "turn_…"}` 时，指明要去掉的用户回合及其之后的所有回合，并优先于 `depth`；Runtime 在线程的恢复保护下解析它，因此其他客户端同时追加的回合不会移动切点。该回合不是此线程的用户回合时返回 `400`
- `POST /v1/threads/{id}/fork-at-turn` - 在一个具名用户回合处 fork（`{"turn_id": "turn_…"}`，即 `GET /v1/threads/{id}` 报告的那个）。该 fork *保留*那个回合及其之前的每个回合，丢弃其后的回合；因此指名最后一个回合会保留整个对话。回执与 `/undo` 相同（`thread`、`original_user_text`、`original_user_images`），携带*第一个被丢弃*的用户回合的提示词——即接下来被问的是什么，即使中间夹着一个无提示词回合（如一次手动 `/compact`）——这样客户端可以把它放回输入区供编辑。源线程、它的会话文档与工作区都不受影响，也没有文件回滚：fork 是一个兄弟对话，而回退工作区会把随之留下的分支一起回退。客户端应当指名回合，而不是计算 `depth`——它们渲染的转录与这里裁剪的回合列表不是同一个列表（引导、仅图片提示词与注入的交接各自只位于一侧），一个差一的客户端计数会在回答 `201` 的同时 fork 错前缀。当该回合不是该线程的用户回合时返回 `400`。
- `POST /v1/threads/{id}/patch-undo` - 回滚被丢弃回合改动过的文件，随后做同样的 fork（`{"depth": N}`）；除 fork 出的线程外还返回 `patch_result`（`files_restored`、`summary`、`snapshot_label`）。所有权、信任、准入与中止规则，以及拒绝时的 `error.code` 取值，参见 [工作区恢复端点](#工作区恢复端点)。
- `POST /v1/threads/{id}/file-revert` - 从线程拥有的一个恢复点中恢复恰好一个文件（`{"path", "snapshot_id", "expected_hash"}`）；永不 fork 对话。参见 [工作区恢复端点](#工作区恢复端点)。
- `POST /v1/threads/{id}/retry` - 以去掉最后 N 个回合的方式 fork，并立即启动一个新回合（`{"depth": N, "prompt": "..."}`；`prompt` 覆盖原始用户文本，省略时复用后者）

`POST /v1/threads/{id}/turns` 接受与逐回合覆盖相同的可选
`reasoning_effort` 与 `allowed_tools` 字段：

```json
{
  "prompt": "Review this change without running tools.",
  "operation_key": "cwc-request-01J7Y6Q9W4",
  "reasoning_effort": "max",
  "allowed_tools": []
}
```

同一个回合上的 `model_provider` / `model_provider_id` 字段会把
该回合经由另一个提供商路由。已保存的线程保持其提供商。
若不带 `model`，该回合使用那个提供商的默认模型（`auto` 线程
保持 `auto`）。该覆盖总是会被预检，并且是
`operation_key` 指纹的一部分。

解析是确定性的：回合覆盖优先于线程默认值，
后者优先于 Runtime 的常规配置。对工具而言，落到常规
配置意味着常规的已配置目录；`[]` 永不被当作
缺失。推理只在精确的提供商/模型路由被解析之后才规范化，
并且即使线程使用固定模型，`auto` 仍然是逐提示词的推理决策。请求仍然进入既有的
`Op::SendMessage` 路径与单一的 `Engine::run_turn` 循环。

图片输入使用同样的回合路径：`"images": [{"mime": "image/png",
"dataBase64": "..."}]`。客户端必须先观察到
`/v1/runtime/info` 中的
`capabilities.turn_image_inputs: true`（或隔离的
Runtime Chat 中继目录）。较旧的 HTTP 运行时会忽略未知字段，所以
文本响应成功并不证明附件被接受了。
该字段为空时会被省略。app-server 的
`thread/message`、`thread/request` 消息与提示词请求也接受它；那个桥
会在转发图片字节之前检查底层 Runtime 能力。
旧式远程 Work 命令不支持图片，并显式拒绝它们。

新的内联图片要求有一个具名模型，且其精确解析后的路由报告
`image_input: "supported"`；Auto 以及未知/不支持的图片路由
会在分类器或提供商分发之前被拒绝。这不会改变
能力未知的路由既有的受信任本地附件行为。
提示词必须非空。输入限制为 10 张图片、每张解码后 4 MiB
字节、总计 5 MiB，以及 8 MiB 的 JSON 请求体。PNG、JPEG、GIF 与 WebP
必须 MIME 匹配、base64 为规范的填充形式，且图片内容有效且有界：
每维最多 8192 像素、总计 33,554,432 像素、解码器分配 64 MiB。
Runtime 不会从这个字段去取路径或 URL。
格式错误的图片会拒绝整个回合；调用方可以保留草稿以便
修正。中继命令轮询使用 8 MiB 的响应预算；发送方
必须按序列化后的字节分页，且不得越过未送达的命令。

被接受的图片字节与顺序会保留在既有的回合记录中，并在重启、导入与 fork 后
重建。重试会保留这些图片，
即使其可选的 `prompt` 改变了文本；撤销响应在存在时包含
`original_user_images`。带图片的记录要求 schema v3，
较旧的读取方会拒绝它。纯文本记录与操作指纹保留
其先前表示。经验证的已存储本地图片保持既有的
每张 5 MiB 上限以及导入/重试时既有的聚合/计数语义；
这套内部存储权限不会
放宽精确的模型或权限检查。图片字节、MIME 与顺序参与请求
身份，因此在同一个操作键下改变图片会冲突。
压缩可以概括更早的上下文；保留原始附件
并不承诺之后每次模型请求都包含它。图片像素不受
文本密钥脱敏约束。

`operation_key` 是一个可选的幂等键，供那些可能在 Runtime 已接受回合后
丢掉 HTTP 响应的客户端使用。它的作用域是当前
Runtime 存储与线程，最多 128 个 UTF-8 字节，且不得
为空、不得包含首尾空白或控制字符。省略它
会保留旧式的“创建一个新回合”行为。

第一个被接受的请求会在发送既有的 `Op::SendMessage` 之前，把该键的 SHA-256 指纹持久地绑定到
Runtime 回合 id 与一份规范请求指纹上。一次精确的重试会在常规的
`{ "thread": ..., "turn": ... }` 响应中返回那个原始回合，并且不会再发出第二次引擎
操作、条目或生命周期序列。在同一线程上用同一个键但不同的提供商/模型、
提示词、推理策略、工具允许名单或
动态工具 schema、环境或权限策略时，会失败关闭并返回
`409 Conflict`。同一个调用方键可以在另一个
线程上独立使用。

Runtime 的私有回合操作索引中只存储限定范围的键指纹、请求指纹、线程 id 与回合 id。
原始键永不被
持久化或记录到日志，且请求体、凭据与附件不会被
复制进该索引。既有的线程/回合持久化仍是进程重启后
返回那个回合的来源。

**精确的已接受回合查询**

`GET /v1/threads/{id}/turn-operations/{operation_key}` 使用与回合提交相同的 Runtime
认证。请对每个路径段做 URL 编码。它返回
`200 OK` 与既有的裸 `TurnRecord`（即 POST
响应中的 `turn` 对象），由那个精确的线程与操作键标识。它不使用
线程的最近回合，也不要求原始请求体或当前路由
设置与之匹配。

- `404 Not Found`：该线程/键不存在绑定，或持久化的身份
  不匹配。这些情况共用同一个泛化响应。
- `409 Conflict`：准入持有该操作声明，或其持久绑定
  不完整。请重试查询；该响应并不授权另一个回合。
- `400 Bad Request`：线程 ID 或操作键格式错误。键使用
  与 POST 相同的 128 字节与空白/控制字符规则。
- `500 Internal Server Error`：存储或既有的声明锁无法
  被安全检查。这并不证明该操作不存在。

该查询在读取绑定与回合时，对既有的操作声明持有一把共享读锁。
它不创建文件、不启动引擎、不发事件，也不做任何回放或恢复。
Runtime 的正常启动可能在之后的某次查询之前恢复一次不完整的准入，但 GET 本身永不做这件事。

**审批**
- `POST /v1/approvals/{approval_id}`，请求体
  `{ "decision": "allow" | "deny", "remember": false }`

`approval_id` 由 Runtime 铸造，而不是由模型或提供商铸造。它是一个
不透明的 `approval_<32 hex>` 能力，每个提示词唯一，绑定到发起它的线程，
且只能使用一次：当决策被送达、当提示词超时、或当回合放弃它时，
Runtime 会移除它。客户端回显它所得到的值，不得自行构造、推导或猜测。

它有意**不是**提供商的工具调用 ID。提供商每次响应都会重置自己的
调用 ID 计数器，因此两个线程可能门控原始 ID 字节级相同的调用；
用那个值作为审批键会让一个线程的决策去解决另一个线程的调用。因此该端点只对铸造出的 ID
做一次精确匹配，且没有回退：一个原始工具调用 ID、一个过期的 ID，或一个已经
被解决过的重放 ID 都返回 `404`，也到不了引擎。`404`
意味着该能力当前并非待决——它不是关于该审批如何被解决的证据；
那要看 `approval.decided`。

Engine 的工具执行 ID 另行以 `tool_call_id` 出现在
`pending_approvals[]` 与审批事件上。它是用于把提示词
挂到它所门控的工具行上的关联符，绝不会被当作决策接受。
每个线程详情中的 `pending_approvals[]` 条目是
`{ "id", "turn_id", "tool_name", "input"?, "description", "intent_summary"?, "tool_call_id"?, "summary"? }`，
其中 `id` 就是上文那个能力。`input` 保存已捕获的工具输入。缺少 `input`
表示没有可用的已捕获值；显式的 JSON `null` 则保留为已捕获的值。
在经过身份验证的规范线程接口上，`approval.required` 事件携带同一份已捕获输入。
`summary`（也出现在 `approval.required` 上）是
对该受门控调用的一行描述，只由工具名与其参数构建，
绝不来自模型文本（“Search the web for 'espresso'”、
“Write notes/espresso.md”）；工作区内的路径为工作区相对路径。
客户端应先展示它，并把原始参数留在其后。对任务与自动化的创建/更新，
`summary` 还会写出所请求的信任模式、shell、自动批准、模式和工作区。
对 shell 命令、任务门禁、shell 输入或校验器，若其 `curl`/`wget` 请求体携带凭据文件
或名称表明是机密的变量，或读取由这两者或 `env`/`printenv` 送入的 stdin，`summary` 显示为
``Upload credentials (`…`) with `…` ``；内置安全闸会拦住该调用，
会话授权或已保存的 allow 规则都无法应答它。超过 64 KiB 或 4,096 个词、且文本中任意位置
含有 `curl` 或 `wget` 的 shell 命令、shell 输入或校验器，以及嵌套过深而无法读取的调用，
同样会被拦住，`summary` 保持普通内容。含有隐藏 Unicode 字符的 shell 命令
会在创建任何审批之前被拒绝。

在 `allow` 上带上 `"remember": true` 会为该工具及其参数类别记录一份**会话授权**
（审批分组键：对简单的已知命令（例如 `git status`，其选项都是 `-s` 或 `--porcelain`
这类不带值的选项）是一个 shell 命令族——复合命令、包装命令、解释器或无法识别的命令，
带有任何其他选项的命令，或其参数就是要运行或安装的东西的命令（`go run`、`make`、
`git bisect`、包安装），按完整的规范化命令授予；shell 交互或等待调用按精确调用授予——
一个 patch 的文件集、一个 `fetch_url` 主机、一个 MCP 工具、一种 `web.run` 动作类型——对
`open` 而言是它打开的那些主机）。Computer Use 同意与 `app_script` 调用，以及
任何没有类别的工具，都只针对那一次精确调用授予。授权永不
改变线程的权限姿态。该线程上之后匹配的调用无需提示词即被批准：
它们仍会发出 `approval.required`，随后是
带 `"auto": true` 与 `grant_id` 的 `approval.decided`。创建授权会
发出 `approval.grant_added`，携带 `{ "grant": { "grant_id", "tool_name",
"scope", "summary", "granted_at" } }`；线程详情会在
`approval_grants[]` 中列出活动授权。`DELETE /v1/threads/{id}/approval-grants/{grant_id}`
撤销其中一个（发出 `approval.grant_revoked`）；下一次匹配的调用
会再次提示。归档或删除线程会结束它的所有授权
（归档会为每个授权发出 `approval.grant_revoked`；取消归档不会
恢复它们）。授权保存在 Runtime 数据目录（`approval_grants.json`）。
Runtime 重启后，线程的授权保持暂停，匹配的调用会再次提示，直到经过认证的客户端
恢复该线程。无法读取的授权存储不含任何授权，因此每次调用都会提示。一个强制的
（不可绕过的）提示永不会被授权回答。

**用户输入**
- `POST /v1/user-input/{thread_id}/{input_id}`，请求体
  `{ "answers": [{ "id": "question-id", "label": "Choice", "value": "Choice" }] }`

提交的值会被送达活动的模型回合，但会被有意
排除在持久 Runtime 条目与事件之外。已结算的工具条目只包含
一份中性回执与一个机器可读的 `response_redacted` 标记。
Runtime 只接受精确待决的 `(thread_id, input_id)` 请求；一个
未知的、正在并发结算的或已经结算的 id 返回 404，且永不会
被放入引擎邮箱。它会在移除快照权威（snapshot-authoritative）的提示
或把答案送达引擎之前，先提交无密钥的
`user_input.answered` 回执。该结算独立于
HTTP 连接运行，因此在提交后断开连接不会留下一个半接受的提示。
终态回合取消通过 `user_input.canceled` 遵循同样的“先回执后
移除”顺序。

**客户端执行的动态工具**
- `POST /v1/threads/{thread_id}/turns/{turn_id}/tool-calls/{call_id}/result`

结果路由中的线程与回合必须与待决调用匹配。一次调用
最多结算一次；错误路由与重复结果返回 404。终态
生命周期事件只携带标识符与状态，绝不携带工具结果内容。
Runtime 会在把提交的结果变为模型可用之前先提交终态生命周期事件。
结果送达、超时与终态回合
取消通过同一个结算所有者竞争，因此对一次调用而言，下列事件中恰好有一个是持久化的：

- `tool_call.requested` — 该带类型的客户端执行调用变成待决；
- `tool_call.resolved` — 结果已被 Runtime 持久接受
  （`result_accepted: true`；`success` 是结果元数据，但结果内容
  被排除）；
- `tool_call.timeout` — 在有界等待到期前没有结果胜出；
- `tool_call.canceled` — 在某个提交结果胜出之前回合已终止。

HTTP `202 Accepted` 与 `tool_call.resolved` 共享“持久接受”这层含义。
两者都不声称模型消费了该结果：一次并发的回合
关闭可能在接受之后关掉模型接收端。一旦 Runtime
接受了结果，该调用就是终态的，重复结果返回 404。

**事件**（SSE 回放 + 实时流）
- `GET /v1/threads/{id}/events?since_seq=<u64>&replay_limit=<n>&progress=true`

游标：

- `since_seq` 是逐线程的游标：发送 `seq > since_seq` 的事件。省略它（且没有
  `Last-Event-ID`）时，流从线程历史的开头开始。
- 每个日志帧都带有 `id: <seq>`，因此浏览器的 `EventSource` 可以通过它在重连时
  发送的 `Last-Event-ID` 头恢复。显式的 `since_seq` 优先于该头，所以一次有意
  从 `0` 开始的回放永远不会被过期的 id 覆盖。不是十进制整数的头值会被忽略。
- `replay_limit`（最多 4096）只返回所请求历史的最新尾部；第一个返回事件上的
  `previous_seq` 会精确越过被省略的那段历史。

持久历史的解析在异步服务器 worker 之外运行，并通过一个有背压的通道，
以最多 256 个事件的有界批次送达 SSE。广播
送达只是一次唤醒优化：一个落后的接收方会从它最后接受的游标
打开同一个有界持久回放。

`progress=true` 会在当前游标处添加 `stream.progress` 传输帧
（`{schema_version, event, kind, thread_id, seq, state}`，`state` 为 `replaying`
或 `live`），并以 `x-codewhale-event-progress: 1` 声明它们。只有在持久历史和
已排队的实时尾部都被排空之后，流才报告 `live`；广播落后后的恢复会让它回到
`replaying`。进度帧永远不携带新的序号。

流打开之前的失败是普通的 HTTP 错误，带 JSON 错误体，从不是 SSE：

| 状态 | 何时 |
| --- | --- |
| `401` / `403` | 缺少 Runtime 凭据或凭据错误 |
| `404` | 未知线程 |
| `400` | `replay_limit` 超过 4096 |
| `500` | 无法打开持久历史（包括第一个游标之前回放 worker 崩溃） |

一旦响应为 `200`，服务器主动选择的每一种结束都是最后一个 `stream.end` 帧；
参见 [结束与恢复线程流](#结束与恢复线程流)。

**快照**（side-git 恢复点列表 + 恢复）
- `GET /v1/snapshots?limit=20`
- `POST /v1/snapshots/{id}/restore`

`/v1/snapshots` 列出运行时工作区最近的 side-git 恢复点。
`limit` 默认为 `20`，且必须在 `1` 与 `100` 之间。`POST
/v1/snapshots/{id}/restore` 从快照恢复工作区文件，
并返回 `{"restored": "<snapshot-id>"}`。它是服务器自身工作区的直接操作者表面
（与 TUI 的 `/restore <N>` 是同一个动作）：它
由 Runtime API bearer 令牌门控，而不是由任何线程的信任标志门控，并且
当有回合在重叠工作区中活动时，它会以 `409` 被拒绝（见
下文）。会先拍一个 `pre-restore:` 安全快照。

```json
[
  {
    "id": "snap_...",
    "label": "post-turn:1",
    "timestamp": 1780730580
  }
]
```

### 工作区恢复端点

有三条路由会从 side-git 快照更改工作区文件。它们共享同一条
准入规则与同一张安全网，区别在于范围与信任。

| 路由 | 范围 | 信任 | 是否 fork 线程 |
| --- | --- | --- | --- |
| `POST /v1/snapshots/{id}/restore` | 整个服务器工作区 | 仅需 bearer 令牌（操作者动作） | 否 |
| `POST /v1/threads/{id}/patch-undo` | 被丢弃回合改动过的文件 | 当文件将被改动时需要线程 `trust_mode` 或 `auto_approve` | 是 |
| `POST /v1/threads/{id}/file-revert` | 恰好一个常规文件 | 总是需要线程 `trust_mode` 或 `auto_approve` | 否 |

**准入。** 一次恢复会预定 Runtime 用于配置重载与会话检查点的同一个准入，
因此在文件被重写期间，没有新回合会启动，也没有已保存的历史会变化。如果任何线程已经在同一工作区、其嵌套检出或其父目录中
有活动回合，请求会以 `409` 被拒绝，消息为
`already has an active turn`。
该预定由执行 Git 变更的 worker 拥有，因此一个在请求中途断开连接的客户端
无法提前释放它；该操作要么整体完成，要么整体失败。并发恢复会串行化。
该预定是运行时级别的：当一次恢复的安全快照与检出在运行时，
每个线程上的新回合、引导、压缩与用户输入送达都会等它结束，
因此一个大工作区可能在恢复期间为其他地方增加数秒延迟。
工作区目录不可用（卷未挂载、共享断开、目录缺失）的线程会以 `409` 被拒绝，
而不是被当作没有东西可恢复。

**所有权。** 一个线程恰好拥有记录在它自己回合上的那些工作区恢复点。回合运行期间，
引擎报告它拍下的每个快照——回合之前的 `pre_turn`；每次可能写入的工具调用（所有
不是只读的调用：文件工具、shell 命令、程序、可写的 MCP 工具）之前的 `tool` 和之后的
`post_tool`；回合结束时的 `post_turn`（总是在回合结算之前）——Runtime 按顺序把它
追加到回合记录的 `workspace_snapshots`：

```json
"workspace_snapshots": [
  { "kind": "pre_turn", "snapshot_id": "<commit>", "tree_id": "<tree>", "session_id": "thr_1a2b3c4d" },
  { "kind": "tool", "snapshot_id": "<commit>", "tree_id": "<tree>", "session_id": "thr_1a2b3c4d", "tool_call_id": "call_…", "write_paths": ["src/lib.rs"], "changed_paths": [] },
  { "kind": "post_tool", "snapshot_id": "<commit>", "tree_id": "<tree>", "session_id": "thr_1a2b3c4d", "tool_call_id": "call_…", "changed_paths": ["src/lib.rs"] },
  { "kind": "post_turn", "snapshot_id": "<commit>", "tree_id": "<tree>", "session_id": "thr_1a2b3c4d", "changed_paths": [] }
]
```

`changed_paths` 列出自回合上一个回执以来内容发生变化的工作区相对路径——即该回执
所关闭的那段时间里发生的事；它在 `pre_turn` 上不存在，无法计算时（中间某个快照失败）
也不存在。`write_paths` 设置在文件工具（`write_file`、`edit_file`、`apply_patch`）的
`tool` 回执上，值为该调用声明的路径，按它给出的写法；没有它的工具（shell 命令）
可能写入任何路径。在用户 shell 回合上，`pre_turn` 回执携带该命令的 `tool_call_id`，
因为命令从它一直运行到 `post_turn`。

每个回执也会作为 `turn.workspace_snapshot` 事件发布（负载就是该回执）。引擎在线程
自己的 id 下运行每个 Runtime 线程，跨越重启和引擎逐出，所以对线程自己运行的回合，
`session_id` 就是线程 id；它不跟随线程的已保存会话绑定（`PUT`/`POST /v1/sessions`、
恢复），后者只是命名一份文档。fork 会克隆其来源的回合记录，因此拥有它继承的那些
回合的恢复点。同一工作区中另一个线程或 TUI 会话的快照永远不是候选。`tree_id` 是
持久身份：修剪会重建 side 仓库并重写每个 commit id，但保留每棵树，而恢复点只解析为
具有相同树、会话标签和种类的已存储快照。每次拍快照后的数量修剪会保留最新的 50 个
快照加上最新的 50 个回合边界（`pre-turn:`/`post-turn:`），所以一个工具调用多于此数的
回合，或来自另一个线程的突发，永远不会把最近回合自己的恢复点挤出去。在回执出现之前
记录的回合、由 `resume-thread` 导入的回合，以及在快照关闭或不可用时运行的回合，
都没有恢复点。

**安全网。** 每次恢复都会先记录当前工作区的一个 `pre-restore:<target>` 快照。
该标签永不会是 `/undo`、`patch-undo` 或
`file-revert` 的候选，因此这张网不会改变之后的撤销选择什么。
对 `file-revert` 而言，备份是强制的：如果它无法被写入，或
请求的文件被它排除（例如被 `.gitignore` 排除），请求
失败且什么都不改变。

**`patch-undo`。** 撤销整个回合。对每个被丢弃回合的 `pre_turn` → `post_turn`
窗口，两个快照之间不同的路径必须全部属于该回合自己：只在该回合某次工具调用的时间段
内改变（一个 `tool` → `post_tool` 时间段，或 shell 回合的整个窗口），并且在文件工具的
时间段内，是该调用声明过的路径。在回合的任何工具都不可能写入它的时候改变的路径——
另一个线程、编辑器、后台进程——是别人的改动，撤销会被拒绝，而不是把它回退。回合的
每个路径都恢复到改动它的第一个被丢弃回合之前的内容，其他任何东西都不碰，所以同一
工作区里用户或另一个线程之后的工作得以保留。然后对话完全按 `/undo` 的方式 fork。

快照从不保存被工作区 `.gitignore` 文件或内置快照排除项（`node_modules/`、`target/`、
`dist/`、构建缓存、二进制产物）排除的路径，也不保存工作区之外的路径。声明了此类路径的
被丢弃文件工具调用会以 `path_not_snapshotted` 被拒绝，因为没有快照能把它放回去。
shell 命令不声明路径：它在被排除路径下写入的东西（构建输出、依赖安装）不在
`patch-undo` 恢复的范围内，也不会被报告。`201` 意味着文件已被恢复
（`files_restored: true`，`summary` 中每个文件一行 `<action> <path>`，
`snapshot_label` 指名 pre-turn 快照），或可证明没有任何东西可恢复
（`files_restored: false`）：每个被丢弃的回合都在这里没有调用工具就运行完、没有改动
文件，或它的文件已经回到回合前的内容。任何无法恢复的情况都会以 `409` 中止整个撤销，
什么都不改变，也不发布 fork；`error.code` 说明原因：

| `error.code` | 含义 |
| --- | --- |
| `restore_point_unavailable` | 某个可能改动过文件的被丢弃回合没有完整记录的恢复点（较旧的记录、由 `resume-thread` 导入、快照关闭，或快照失败） |
| `restore_point_pruned` | 恢复点已不在快照存储中 |
| `path_not_snapshotted` | 某个被丢弃的文件工具调用写入了快照不保存的路径（被忽略、内置排除，或在工作区之外） |
| `workspace_changed_since_turn` | 这些回合改动过的某个路径之后又被改动（或在两个被丢弃回合之间被改动）、某个路径在被丢弃回合运行期间但在其自身工具调用之外被改动，或某个路径不是常规文件 |
| `restore_requires_trust` | 有东西要恢复，而线程不处于受信任模式或 Full Access |
| `workspace_unavailable` | 工作区目录不可用 |

对前四种情况，客户端可以改为提供只作用于对话的 `POST /v1/threads/{id}/undo`，
以及针对单个文件的 `file-revert`。快照仓库、列表或比较失败以 `500` 中止，同样保留
对话，因此一个回合永远不会在其文件改动仍留在磁盘上时被丢弃。深度与历史在任何文件
改动之前都被校验。如果文件已恢复后 fork 无法被持久化，响应是一个 `500`，并点名被
恢复的那个快照；原线程仍持有该回合，而 `pre-restore:` 快照持有先前的文件。

**`file-revert`。** 请求体：

```json
{
  "path": "src/lib.rs",
  "snapshot_id": "3f2a…40-or-64 hex…",
  "expected_hash": "sha256:<64 lowercase hex digits>"
}
```

- `path`：工作区相对路径，或线程工作区内的绝对路径。该
  名称是字面的（方括号、空格与 glob 字符都是文件名字节；
  Git 以 `--literal-pathspecs` 运行）。它必须指名一个常规文件：目录、
  路径中任何位置的符号链接以及 `.git` 组成部分都返回 `400`。
- `snapshot_id`：用户所选那次改动的精确 `tool` 或 `pre_turn` 恢复点，来自线程自己的
  回合记录：某个回执的 `snapshot_id` 或 `tree_id`（对工具调用而言，是 `tool_call_id`
  匹配的那个回执），或 `GET /v1/snapshots` 为它列出的当前 commit id。它必须是记录在
  该线程某个回合上的恢复点；服务器永不自行挑选“最新的不同快照”，因为一个不相关的
  较新快照可能在保留工具改动的同时抹掉之后的用户编辑。
- `expected_hash`：客户端展示的当前文件字节的 `sha256:`，
  或当客户端看到该文件为已删除时的 `absent`。它会在
  安全备份之前、以及在变更前一刻各检查一次。

响应：

- `200 {"path", "action", "snapshot_id", "snapshot_label"}` —— `action` 为
  `modified`、`recreated`（文件此前缺失）或 `removed`（该快照
  不包含该文件，因此工具创建的文件被删除；其父
  目录留在原处）。
- `400`：格式错误的 `snapshot_id`/`expected_hash`、路径在工作区之外，
  或两侧中任一侧不是常规文件的路径。
- `404`：未知线程。
- `409`：线程未处于受信任模式或 Full Access；重叠工作区中有活动回合；
  工作区目录不可用；快照未知、已被修剪、未记录在该线程的回合上（属于另一个线程
  或某个 TUI 会话），或不是恢复点（请刷新改动记录）；文件已经与快照
  一致（没有东西可回退）；或文件在被审阅的
  `expected_hash` 之后发生了变化（请刷新并重新审阅）。在这些情况下
  什么都不会被改动。
- `422`：请求体字段缺失或类型错误。
- `500`：Git 或文件系统失败；在安全快照之后的失败会点名
  那个快照，以便用
  `POST /v1/snapshots/{id}/restore` 或 `/restore` 恢复先前的字节。

能力探针：对这条路由做 `GET`，在端点存在的地方返回 `405`，
在较旧引擎上返回 `404`；客户端把任何非 `404` 都视为可用，
否则以说明降级。

**兼容流**（一次性、向后兼容）
- `POST /v1/stream`

**任务**（持久后台工作）
- `GET /v1/tasks`
- `POST /v1/tasks`
- `GET /v1/tasks/{id}`
- `POST /v1/tasks/{id}/cancel`

**自动化**（按计划重复执行的工作）
- `GET /v1/automations`
- `POST /v1/automations`
- `GET /v1/automations/{id}`
- `PATCH /v1/automations/{id}`
- `DELETE /v1/automations/{id}`
- `POST /v1/automations/{id}/run`
- `POST /v1/automations/{id}/pause`
- `POST /v1/automations/{id}/resume`
- `GET /v1/automations/{id}/runs?limit=20`

创建与更新请求接受一个可选的 `model`。存在时，每次
按计划或手动触发的运行都使用该模型；省略它则保持
运行时默认的任务模型。

**Operate**（常驻的具名操作；与 CWC
`20de981` / PR #284 相同的 `OperateRecord`）

- `GET /v1/operate` — 当前操作 + 计划看板
- `POST /v1/operate` — 创建（`direction`，可选 `burnRate`）
- `PATCH /v1/operate` — 引导方向、`burnRate` 或 `leadPlan`
- `PUT /v1/operate/plan` — 设置 `leadPlan`（`{ slices: [...] }`）
- `POST /v1/operate/keepalive` — 观察消耗 / 燃烧；永不停止
- `POST /v1/operate/cancel` — 显式取消（`/v1/operate/stop` 为别名）；
  同时暂停 `cw-operate` keepalive，以免取消后还有东西继续花钱
- `POST /v1/operate/auto-merge/check` — 调用已落地的
  `scripts/check-auto-merge.py --repo --pr --agent`（不做合并）

操作记录（`current.json`）在跨进程文件锁下持久化，
采用临时文件 + 重命名的原子写入；每次 PATCH / keepalive / 计划
保存都会在锁内重新加载最新状态，因此并发保存会合并
而不是丢失写入。一个改变 `direction` 的 `PATCH` 会使
已记录的 `leadPlan` 失效（worker 停止执行被取代的切片），并
把 keepalive 的 lead 运行提前以重新规划。`POST /v1/operate`
会装上每小时的 `cw-operate` keepalive，并立即启动它的第一次 lead-plan
运行，而不是等完第一个周期；凭据
通过常规的 Z.ai 提供商解析获得（配置、`api_key_env`、
密钥存储或提供商环境变量——空值算作缺失）。

`burnRate` 是 `{ "kind": "usd_per_hour", "amountUsdPerHour": number }`，
一个正数，或 `null`（无上限）。状态为
`planning | running | idle_blocked | cancelled`。节奏
（`unbounded | hold | throttle | widen`）不是状态：超目标
则限流，低于目标则放宽，没有钱包上限式的停止。仅当
direction 为空、等待 lead 计划、缺少凭据或有
人工门控时才是 idle-blocked。自动合并是来自 codewhale-ops `origin/main` 的
`scripts/check-auto-merge.py --repo … --pr …
--agent …`（退出码 0），然后是
`scripts/auto-merge-pr.py`。不要再发明第二个检查器。

**工作区信任**（持久化的项目决定）
- `GET /v1/workspace/status` 返回 `workspace_trusted`：服务器工作区是否已在发现的用户配置中被设为可信。项目技能、命令和 MCP 配置会检查这个决定。项目钩子还需要单独批准钩子文件的精确摘要。
- `POST /v1/workspace/trust` 接受 `{"trusted": true|false}`，记录决定并返回 `{workspace, workspace_trusted, config_path, message}`。

处理器复用 `/trust on|off --save` 使用的 `config::set_workspace_trust`，再通过 `is_workspace_trusted` 读回结果。请求不接受路径，只作用于服务器已接纳的工作区。`config_path` 标识实际写入的文档。读写都使用默认配置发现规则，包括已配置的环境变量覆盖；启动运行时的显式 `--config` 参数不会选择这个信任文档。

项目技能和命令会在各自现有的加载或回合边界检查信任。项目钩子还需要单独的摘要批准，并在执行前重新检查权限。重启引擎可重新建立项目 MCP 连接；本请求不会断开已有连接池或中断正在运行的回合。现有上下文加载器会独立于此标志加载 `AGENTS.md` 等项目指令文件和项目规则，因此撤销工作区信任不会移除这些指令或已持久化的历史。

这个持久化的项目决定与线程的 `trust_mode` / Full Access，以及 `/trust add <path>` 的外部路径授权分别独立。`/trust on|off --save` 同时修改会话文件工具信任和持久化项目记录；本 HTTP 路由只修改项目记录。POST 使用其他状态变更路由相同的运行时写权限；watch 设备令牌可以读取状态，但不能写入信任。

**内省**
- `GET /v1/workspace/status`
- `GET /v1/workspace/files/search?query=<partial>&limit=<1-100>`（参见上文的工作区文件建议）
- `GET /v1/workspace/files?path=<dir>&limit=<1-2000>`、`GET /v1/workspace/files/read?path=<file>&offset=&limit=`
  与 `PUT /v1/workspace/files`（参见上文的工作区文件与会话工件）
- `GET /v1/skills`
- `GET /v1/skills/{name}` — 返回路由元数据（`source`、`invocation`、`aliases`、
  `bundled_tier`、`enabled`）及完整 `SKILL.md`，让客户端为自己的下一回合组成激活指令。
  不存在的技能或已删除的原生文件返回 404；权限已过期的插件快照返回 403。
  客户端先查 `GET /v1/runtime/info` 的 `capabilities.skill_detail`；单凭 404
  无法区分“没有这项技能”与“没有这条路由”。

- `GET /v1/apps/mcp/servers`
- `GET /v1/apps/mcp/tools?server=<optional>`

技能激活开关在跨进程事务锁下持久化。
每次变更都会在原子写入前重新加载并合并最新的精确名称状态，
而 `GET /v1/skills` 会刷新那份共享状态，使另一个 Codewhale
进程的成功切换无需重启 Runtime API 就能可见。

**用量**（跨线程的 token/成本聚合）
- `GET /v1/usage?since=<rfc3339>&until=<rfc3339>&group_by=<day|model|provider|thread>`

`since` / `until` 是含端点的 RFC 3339 时间戳，可以省略（无
边界）。`group_by` 默认为 `day`。桶按键升序排序。
空时间范围产生空 `buckets`（绝不会是 404）。成本通过
模型→定价映射计算；模型没有定价条目的回合贡献
token 但成本为 `0.0`。于 v0.8.10 加入（#564）。

```json
{
  "since": "2026-04-01T00:00:00Z",
  "until": "2026-04-30T23:59:59Z",
  "group_by": "day",
  "totals": {
    "input_tokens": 12345,
    "output_tokens": 6789,
    "cached_tokens": 0,
    "reasoning_tokens": 0,
    "cost_usd": 0.012,
    "turns": 42
  },
  "buckets": [
    {
      "key": "2026-04-30",
      "input_tokens": 1234,
      "output_tokens": 678,
      "cached_tokens": 0,
      "reasoning_tokens": 0,
      "cost_usd": 0.001,
      "turns": 3
    }
  ]
}
```

### 原生客户端路由

这些族在同一套 bearer 令牌传输上为原生客户端服务。它们复用运行时既有的权威——
引擎的 shell 管理器、持久线程存储、工作区限制层、
配置的凭据管道——并不增加第二套运行时、会话存储、
调度器或凭据存储。

**终端会话**（持久、由 Engine 拥有的 shell）

上面的 jobs 族每个作业运行一条命令。终端面板需要的是
**另一种**权威：智能体自己的终端工具所驱动的、有状态且基于 PTY 的 shell，它在多次输入之间保持 cwd 与环境。
这些路由附着到那个会话，且从不创建会话——一个没有活动会话的名字返回
`404`，因为从一次 HTTP 请求中凭空变出一个 shell 会让客户端得到一个
Engine 并不知道的终端。输入可按路由归因：
`input` 是客户端的写入通道，`terminal_send` 是智能体的。

- `GET /v1/terminal/{name}/output?cursor=<bytes>&max_bytes=<1-64KiB>&format=
  <base64|text>` — 可恢复的字节流。`{name, offset, next_cursor,
  total, dropped, encoding, data, running, exit_code}`：把 `next_cursor`
  传回来即可继续；读取永不消费，因此多个客户端可以持有
  各自独立的游标；`dropped` 报告 512 KiB 环形缓冲区丢弃的字节数，
  而超出 `total` 的游标会从 `total` 作答，而不是把它回显回来
- `POST /v1/terminal/{name}/input` — `{ "data", "encoding"? }`，默认
  `base64`（精确字节），或用 `text` 传 UTF-8 → `{ "name", "written" }`
- `POST /v1/terminal/{name}/resize` — `{ "rows", "cols" }` → 子进程绘制目标的内核
  窗口
- `POST /v1/terminal/{name}/kill` — 结束该 shell；通过
  `output`（`running` / `exit_code`）观察退出，而不是看这次确认

`GET /v1/runtime/info` 声明 `terminal_stream`、`terminal_input`、
`terminal_resize` 与 `terminal_kill`。这四个在 Windows 与 OpenHarmony 构建上
目前都是 `false`：其所有者仅支持 Unix，那些路由回答 `501`，因此客户端
应当用这些布尔标志来门控终端控件，而不是通过一次失败请求去发现这一点。
以下限制需要明说，否则读者会自行假设：没有 `wait_ms` 长轮询（请轮询游标），
被环形缓冲区丢弃的回滚内容随进程一起消失，重启后的 Engine
会报告没有会话，而不是假装重新附着；且
`@codewhale/runtime-sdk` 包还没有终端客户端封装——目前
裸路由就是契约。

**作业**（操作者范围内的 shell 作业；终端表面）
- `GET /v1/jobs` — 跨所有线程的每个活动与已知过期作业
- `GET /v1/threads/{id}/jobs` — 由一个线程的管理器拥有的作业：
  模型启动的、子智能体启动的与客户端启动的合在一起
- `POST /v1/threads/{id}/jobs` — `{ "command", "cwd"?, "timeout_ms"?,
  "tty"?, "env"? }` → `201 { "job" }`；在线程投影出的沙箱策略下作为后台 shell 运行。
  `tty: true` 会把 stderr 合并进 stdout，
  并给命令一个终端（交互式程序必需）；
  后台作业永不会在 `timeout_ms` 时被杀掉。相对 `cwd` 在线程工作区内解析。
  未开启信任模式时，解析符号链接后的 `cwd` 必须仍在该工作区内，否则返回 `403`：
  与 shell 工具不同，此路由不采用 `workspace_follow_symlinks` 或 `/trust add` 根目录，
  所以指向工作区外的符号链接会被拒绝。作业在已解析并检查过的目录中运行，
  之后重定向符号链接不会改变其运行目录；`cwd` 解析为非 UTF-8 路径时返回 `400`
- `GET /v1/threads/{id}/jobs/{job_id}` — 单个作业的状态 + 元数据
- `GET /v1/threads/{id}/jobs/{job_id}/output?stream=<stdout|stderr>&cursor=
  <bytes>&max_bytes=<1-512KiB>&wait_ms=<0-30s>&format=<base64|text>` —
  可恢复的字节流。`{job_id, stream, offset, next_cursor, total,
  dropped, encoding, data, status, exit_code, done}`：把 `next_cursor`
  传回来即可继续；`wait_ms` 在运行中的作业上长轮询等待新字节；
  `done` 意味着终态且游标之后不再有内容
- `POST /v1/threads/{id}/jobs/{job_id}/stdin` — `{ "data", "encoding"?,
  "close"? }`：`data` 默认为 UTF-8 文本，或用 `base64`；`close: true`
  发送 EOF；对 PTY 与管道作业都适用 → `204`
- `POST /v1/threads/{id}/jobs/{job_id}/kill` — 在进程组上做有界的 SIGTERM → SIGKILL
  升级 → `{ "job", "result" }`，带最终快照

读取是非消费式的：多个客户端可以持有各自独立的游标，而
轮询永远不会从引擎自己的增量消费方那里偷走输出。缓冲区
是有界的，并有精确的丢弃核算——一个 `cursor`
落在保留窗口之后的读取方会得到越过它的 `offset` 与 `dropped > 0`，
并且必须重新锚定。被淘汰的作业保留一份尾部快照，输出
路由就把它作为最终保留窗口提供。作业限定于创建它的
线程，并会在该线程被移除时被杀掉；引擎的
后台命令使用同一个逐线程管理器，因此 `GET /v1/jobs`
也是客户端看到模型派生工作的地方。

**命令**（带类型的命令目录，APPS-28）
- `GET /v1/commands` — `{commands: [...]}`：TUI 自己的注册表所持有的
  每一个已注册斜杠命令，内置与用户自定义的都在内。每个条目包含：`name`、
  `aliases`、`summary` 与 `usage`（英文源文本——本地化是
  客户端的表面）、`subcommands`（usage 行声明的字面动词）、`takes_arguments`、`kind`（`builtin` 为已注册代码，或 `user`
  展开一个已存储模板）、`binding`（`host` 在本地运行且
  永不到达模型；`prompt` 展开进模型看到的请求）、
  `discovery`（`primary` / `advanced` / `compatibility`，仅内置）、
  `hidden` 用于产品不对外宣传的行，以及 `shadowed_by` /
  `shadowed_aliases`，用于某个用户命令占用了内置命令的拼写的情况。

  每个条目还携带输入区参数形态，按
  TUI 输入区的计算方式算出，这样客户端就不必从 `usage`
  重新推导它（#6230）：
  - `requires_argument` — usage 行提到了任何参数，无论是必需还是
    可选。
  - `requires_required_argument` — usage 行中有落在每个 `[optional]` 组
    之外的 `<required>` 参数。
  - `composer_wants_trailing_space` — 接受该命令后会在其参数前
    留一个尾随空格。
  - `palette_runs_directly` — 命令面板在选择时直接运行该命令，
    而不是把它粘进输入区。
  - `show_in_empty_discovery` — 当斜杠菜单在无筛选文本下打开时
    列出该命令。

  用户命令从 `takes_arguments` 推导这些：它们的参数
  永不是必需的，接受参数的模板会在输入区等待，
  而不接受参数的模板则直接运行，且 `hidden` 模板不会出现在空
  发现结果中。

  这是 TUI 命令面板读取的同一个注册表，因此桌面命令面板可以
  对照它检查，而不是与它逐渐偏离。客户端必须遵守的两条规则：
  `binding: "host"` 的行永不会被作为模型提示词提交，且
  占用内置名称的用户命令在该拼写上胜出。

**钩子**
- `GET /v1/hooks[?thread_id=...]` — `{workspace, enabled, hooks: [...],
  problems: [...]}`：Runtime API 线程为该工作区运行的钩子集合
  （服务器工作区，或具名线程的工作区）。每个条目包含：`name`、`event`、
  `command`（凭据形态的值会被掩码；每个 URL 只保留其 scheme 与 host）、`background`、`timeout_secs`，
  以及 `source`（`global` 用户配置、`plugin` 已审阅插件、`project`
  已信任并批准的 `.codewhale/hooks.toml`）。`problems` 逐行列出一加载时
  被拒绝或告警的钩子。

  每个 Runtime 线程都用这套集合构建自己的引擎：`tool_call_before`
  可以拒绝一次调用，`shell_env` 会作用于 shell 工具，而 `tool_call_after`
  与 `on_error`（针对失败的工具）作为观察者触发，与 TUI 中一样。
  客户端读取这条路由，而不是自己维护一份钩子表。

**上下文**（逐线程上下文压力，APPS-90）
- `GET /v1/threads/{id}/context` — `input_tokens`（可见量表所用、
  保守的实时估算）、`billed_input_tokens`（存在时，最近一次由提供商计数的提示词大小）、`window_tokens`、
  `output_cap_tokens`、`input_budget_ceiling`、`available_input_tokens`、
  `compaction_trigger_tokens`、`usage_percent` 与 `pressure`。由
  活动引擎通过 `Op::GetContextBudget` 提供。每个数值字段都可为空——
  一条无法表达有界窗口的路由会报告 `null`，而不是编造一个数字——
  而 `live: false` 标记那些引擎无法被加载、只有存储中记录的该路由的静态窗口
  被解析出来的响应。

**Git**（工作区仓库操作，APPS-106）
- `GET /v1/git` — 状态详情：`git_repo`、`branch`、`head`
  （缩写，仅供展示）、`head_oid`、`index_token`、`revision`、
  `ahead`/`behind`、计数、逐文件的 porcelain `files[]`
  （`{path, index, worktree, staged, status, old_path?, rev}`）、`branches`、
  `remotes`。`files[].path` 与 `old_path` 是工作区相对路径，与写入路由使用同一坐标系；
  当工作区是其仓库的一个子目录时，工作区之外的行不会列出（计数仍是整个仓库的）。
  移出工作区的重命名显示为其来源的删除。普通 Git 过滤器和未跟踪设置照常生效。
  未跟踪目录保持折叠；只有指名工作区本身的那一行会展开为可逐个寻址的文件。
  前置条件令牌是不透明的：
  - `head_oid` — 完整的 HEAD commit id；在未出生分支上或无法读取 HEAD 时为 `null`
  - `index_token` — 整个索引（每个条目的模式、blob、stage 与路径，覆盖整个仓库）。
    `git status` 只刷新 stat 信息时不会改变它。仓库读取失败时状态仍是尽力而为；
    不可用的令牌为 `null`，损坏的 HEAD 无法满足守卫
  - `files[].rev` — 一行：它的索引条目，加上它覆盖的每个文件的工作树状态（包括
    重命名在工作区内的来源）。内容令牌（`c-…`）包含文件字节、Unix 上的可执行位、
    符号链接目标，以及子模块的 HEAD/状态。子模块的脏内容由 porcelain 状态概括，
    不做递归哈希；普通的 stage/discard 不会写入这些内容。一次读取最多哈希
    64 MiB / 4,096 个文件；超出该预算或包含超过 16 MiB 文件的行，携带仅供展示的
    大小加修改时间令牌（`s-…`）。这些令牌不能守卫写入。不可读路径、位于符号链接
    目录之下的路径、特殊文件和损坏的嵌套仓库的 `rev` 为 `null`；其他行保留各自的令牌。
    如果一个损坏的已跟踪子模块让 porcelain 中止，普通行会在不递归子模块的情况下
    恢复，不可读的子模块显示为 `status: "unknown"`、`worktree: "?"`
  - `revision` — 整棵树：`head_oid`、`index_token`、每一行的 `rev`，包括子目录工作区
    之外的行的工作树状态。当任何一行是仅 stat 或不可读、某次仓库读取不完整，或未跟踪
    路径被隐藏时为 `null`；这种情况下整棵树的受守卫写入不可用。客户端不得静默省略守卫
- `GET /v1/changes` — 同一个 porcelain `files[]` 投影，加上 `head_oid`、
  `index_token` 与 `revision`，只是去掉了仓库外壳（branches/remotes）：
  只有一份权威，因此改动列表永不会与状态读取不一致
- `GET /v1/diff?path=` — 单个文件相对 `base`（`HEAD`，
  或在未出生分支上的空树——它把已暂存的新增读作新
  文件）的统一 `diff`。一个 patch 覆盖已暂存+未暂存；`truncated` 报告
  512 KiB 上限。未跟踪文件会回答 `untracked: true` 与一个空的
  diff——客户端自己去读该文件，而不是把它误当作
  未改动
- `GET /v1/workspace/diff?limit=` — 整棵树的 patch（默认 256 KiB，
  最大 4 MiB）加一份完整的 `--numstat` `files[]` 清单
  （`{path, added, deleted}`），这样即使 patch 被截断，每一行改动的文件也能渲染
- `GET /v1/git/graph?limit=` — 有界的 commit 行（`id`、`short`、
  `parents`、`author`、`timestamp`、`refs`、`subject`）；未出生分支是
  一张空图，不是错误
- `POST /v1/git/stage` `{ "paths": [...] }` 或 `{ "all": true }`；
  `POST /v1/git/unstage` 相同；`POST /v1/git/discard` `{ "paths": [...] }`
  （仅已跟踪路径——没有 `all`，未跟踪路径失败关闭）；
  `POST /v1/git/commit` `{ "message", "all"? }`；stage、unstage、discard
  与 commit 还接受一个可选的 `expect`（见下文）；`POST /v1/git/push`
  `{ "remote"?, "set_upstream"? }`（`remote`，或仅提供 `set_upstream` 时使用的 `origin`，必须是已配置的远端名称）；`POST /v1/git/branch`
  `{ "name", "create"? }`

diff 与前置条件令牌的读取通过加固过的审阅命令运行（过滤器、fsmonitor、钩子、
惰性抓取与 replace-objects 均被中和）。porcelain 状态使用普通 Git，与工作区的计数和
过滤器一致；写入通过非交互式命令路径运行（`GIT_TERMINAL_PROMPT=0`、BatchMode ssh），
因此凭据或主机密钥提示永不会挂住一个请求。路径列表是工作区相对的，并受与文件路由
相同的限制（穿越 → 400，`.git` → 403），在 `--` 之后以 `--literal-pathspecs` 传入，
所以 `src/*` 指名一个叫 `*` 的文件，永远不是 glob。（对整棵树的 unstage 使用 `:/`
根 pathspec。）变更操作回答 `{ok, output, status, current}`：刷新后的状态与完整的
`GET /v1/git` 详情，因此客户端在一次操作后不需要再读取任何东西，并可以用新的令牌
串接下一次写入。不是仓库的工作区回答 `404`。

*前置条件。* stage、unstage、discard 与 commit 接受
`expect: { head?, index?, revision?, files? }`，由最近一次 `GET /v1/git` 构建。
出现的字段会被检查，缺失的不会；`head: null` 表示“HEAD 必须仍是未出生的”。
不带 `expect`（或带 `expect: {}`）时，写入的行为与以前完全一样。推荐用法：

| 操作 | `expect` |
| --- | --- |
| stage / unstage `paths` | `{head, files: {path: rev}}` |
| stage / unstage `all` | `{head, revision}` |
| discard（总是——它会销毁编辑） | `{head, files: {path: rev}}` |
| commit | `{head, index}` |
| commit `all` | `{head, revision}` |

格式错误的前置条件在任何操作运行之前就回答 `400`：`head` 必须是 40 或 64 位十六进制
id 或 `null`；`index` 与 `revision` 为 64 位十六进制（显式的 `revision: null` 会被拒绝）；
`files` 的值必须是内容安全的 `c-` rev（`s-` 令牌回答 `400`）。`files` 的键
（工作区相对；`dir/` 与 `dir` 是同一个键）必须恰好指名所请求的路径，这样就不会有路径
意外地没有守卫。`files` 与 `all: true` 一起时会被拒绝（请用 `revision`），在 commit 上
也会被拒绝。`expect` 内的未知键与其他未知字段一样被拒绝。当仓库已不再匹配时，路由
什么都不写，并回答 `409`：

```json
{ "error": { "message": "The repository changed since it was read (HEAD moved; src/a.rs changed). Nothing was written; refresh and review again.",
             "status": 409, "code": "git_state_changed" },
  "stale": ["head", "files"], "stale_paths": ["src/a.rs"],
  "current": { "...": "the GET /v1/git detail" } }
```

`stale` 列出移动了的组成部分（`head`、`index`、`files`、`revision`）；`stale_paths`
列出 `rev` 发生变化的 `files` 键。客户端从 `current` 重新渲染，保留用户的选择，
然后再次询问。

来自同一个运行时的 stage、unstage、discard、commit 与 branch 是串行化的，因此一次
检查和它的写入相对于该运行时的其他窗口是原子的；第二个并发写入会回答 `409`，
`error.code: "git_busy"`，而不是排在一个很长的 commit 钩子后面。push 不串行化：它只
移动远程 ref，并且可能为网络等待最多 120 秒。该锁不覆盖此运行时之外的进程——终端、
编辑器，或 Codewhale 自己的智能体工具——它们仍可能在检查与 git 取得 `index.lock` 之间
的那一刻改动仓库；真正并发的 git 写入随后会在 git 自己的 `index.lock` 上失败（一个携带
git 消息的 `400`）。通过 `commit-tree` 与 `update-ref` 做比较并交换的 commit 可以关闭
这个窗口，但会跳过仓库的钩子，而审阅面板的 commit 必须运行这些钩子，所以没有采用。

**诊断**（只读日志、崩溃、进程——APPS-103）
- `GET /v1/logs` → `{sources: [{dir, files: [{name, size, modified}]}]}` —
  运行时的日志目录，加上来自 codewhale home 的 `audit.log[.1]`，
  最新的在前，有上限
- `GET /v1/logs/{name}?offset=<bytes>&limit=<bytes>&tail=<bytes>` →
  `{name, size, modified, offset, bytes, truncated, encoding, content}` —
  一个有界窗口；`tail` 从末尾读取，与 `offset` 互斥；
  `truncated` 意味着返回窗口之后还有字节
  （在 EOF 处做 tail 读取为 `false`），`encoding` 为 `utf-8` 或 `base64`
- `GET /v1/crashes`、`GET /v1/crashes/{name}` — 在崩溃转储目录上使用同样的
  列表/读取契约（`~/.codewhale/crashes`，合并旧式的
  `~/.deepseek/crashes`）
- `GET /v1/process` → `{pid, version, commit, started_at, uptime_seconds,
  executable, rss_bytes}` — `rss_bytes` 只在平台报告它的地方才有
  （Linux `/proc`）；其他地方是缺失，而不是编造

这些路由把磁盘上已有的东西打包，供客户端侧导出；
没有遥测上传路由，也没有第二套日志存储。名称会做
basename 校验（无分隔符、无 `..`），列表有上限，读取是
有界窗口，且符号链接永不被跟随——客户端自己打包那些
文件。

**目标与远程姿态**（APPS-50）
- `GET /v1/targets` → `{targets: [self], remote: {supported: true,
  attach: "client", probe: "POST /v1/remote/connect"}, ssh: {…},
  cloud: {…}}` — 本运行时自己的记录作为可附着目标，加上
  逐表面的所有权；运行时不保留持久目标注册表，
  因此 `POST /v1/targets` 与 `POST /v1/targets/switch` 回答
  `501 Not Implemented`——目标选择由客户端拥有，且一次切换
  绝不能把运行中的任务搬到服务器侧
- `GET /v1/remote` → `{bind_host, port, loopback_only, reachable_from_lan,
  auth_required, mobile, tls}` — 本监听方的可达性姿态。
  `tls` 总是 `false`：该 API 没有 TLS 终止器，因此非回环
  可达性假定了经过验证的 overlay（VPN/mesh），而不是裸的 LAN 信任
- `POST /v1/remote/connect` `{ "endpoint": "http://host:port" }` — 探测一个
  候选远程端未经认证的 `GET /v1/runtime/info`（仅取源站；
  粘贴进来的任何路径都被丢弃）。成功时回答 `{ok, remote: {endpoint,
  runtime_api_version, codewhale_version, auth_required, …}, attach:
  "client"}`，失败时以数据形式回答 `{ok: false, reason: "unreachable" |
  "not a Codewhale runtime" | …}`。携带凭据的 URL
  会以 400 被拒绝——远程端的令牌是在客户端侧配置的，而一条会转发令牌的连接路由
  将是一个数据外泄原语
- `GET /v1/ssh`、`GET /v1/cloud` → `{supported: false, owner:
  "codewhale-control-plane", reason}`；`POST /v1/ssh/connect` 与
  `POST /v1/cloud/attach` → `501`：SSH 工作区供应与托管式
  云电脑属于 Apps 控制面（Managed Computer 的 ASCII Box），
  而不是 Core 内的第二套权威

远程 Codewhale 就是一个带令牌的 `serve --http` 运行时——这就是
整个附着模型。这些路由描述并探测它；它们永不在本地机器上执行
远程请求。

**LSP**（工作区语言智能，APPS-93）
- `GET /v1/lsp` — 能力：`enabled`、受支持且带各自
  服务器命令的 `languages`、`custom_languages`、操作、轮询与诊断上限
- `GET /v1/diagnostics?path=` — 文件诊断
- `GET /v1/definition?path=&line=&character=`（从 1 开始）
- `GET /v1/references?path=&line=&character=`（从 1 开始）
- `GET /v1/symbols?path=&query=` — 空 query 返回文档符号

一个惰性构建的工作区级 `LspManager` 为这些路由服务；引擎线程
为编辑后钩子保留各自的逐线程管理器，而一个
从不服务任何 LSP 路由的服务器也不会启动语言服务器。`path` 是
工作区相对的，并受与文件路由相同的限制。正常缺席也是数据：没有语言服务器、
`[lsp]` 配置被禁用，或一次超时，都会回答 `200`，带 `ok: false` 与机器可读的 `reason`
（`no_server`、`lsp_disabled`、`lsp_error`）；格式错误的输入是 400，
文件缺失是 404。

**语音**（宿主听写，APPS-98）
- `GET /v1/voice` — 能力：`available`、检测到的 `recorder` 命令、
  解析后的 `asr` `{kind, model}`、`modes`、`send_phrases`、
  `max_record_seconds`
- `POST /v1/voice/dictate` — 录音后转写 → `{ ok, text }`
- `POST /v1/voice/send` — 同样的采集，但使用“send it” / 发送/發送
  后缀契约：`send: true` 告诉客户端提交（空的 `text`
  加 `send: true` 意味着提交客户端当前的草稿）
- `POST /v1/voice/control` `{ "composer": "draft text" }` — 辅助
  听写，把输入区文本展示给模型；响应中的 `assisted: false` 意味着一个
  免费的 ASR 后端（本地 whisper/Groq）处理了音频，且输入区上下文从未被看到

运行时拥有宿主麦克风与 ASR 分发——与 TUI 的 `/voice` 命令所运行的是同一套实现，只是无头运行。
录音是每台主机一次阻塞式采集（请求串行化；输家得到
`ok:false`/`no_speech`，而不是一个被争抢的设备）。提供商 ASR 惰性解析其
密钥，因此本地 whisper 与 Groq 路径无需提供商认证即可工作。
中间和最终转写均使用已选择的 ASR 后端。本地 whisper 或 Groq 失败时，
错误保留在该后端；运行时不会把录音或输入区文本改发给当前模型提供商重试。
如需使用该提供商，必须显式选择提供商 ASR。
失败也是数据：`no_recorder`、`no_speech`、`no_provider_auth`、
`transcription_failed`。`CODEWHALE_DISABLE_VOICE=1` 是操作者
开关——无头的 `serve --http` 主机会报告 `available: false`，
并且每次听写调用都失败关闭。

## 提供商与模型选择

这三条路由是 GUI 渲染模型选择器的方式，使其内容对*本*运行时
为真，而不是从某个版本快照猜出来的。它们在 2026-08-04 之前没有文档，
这让一个桌面集成付出了一天的代价：客户端探测了 `/v1/models`、`/v1/runtime/models`
与 `/v1/runtime/providers`（都正确地返回 404），并得出该能力
不存在的结论。

### `GET /v1/providers`

```json
{
  "current": "modelstudio-token-plan",
  "providers": [
    {
      "id": "modelstudio-token-plan",
      "model_provider_id": "modelstudio-token-plan",
      "display_name": "Alibaba Cloud Model Studio",
      "default_model": "qwen3.8-max",
      "has_model_catalog": true,
      "credentialState": "configured"
    }
  ]
}
```

`current` 是活动的通用提供商 id。只有活动条目携带精确身份：活动的内置提供商
通常会在 `model_provider_id` 中重复其规范 id，而活动的具名自定义路由会把 `current` 设为
`custom`，并在那里给出精确的已配置键（例如 `lm-studio`）。其他
条目的精确 id 为 null；活动 `custom` 条目上的 null id 标识
已发布的旧式根级自定义路由。请从所选条目保留这两个字段，
并把非 null 的精确 id 作为 `POST /v1/threads` 的
`model_provider_id` 回传；丢掉具名自定义 id 会把选择
塌缩到旧式的根自定义路由。`credentialState` 是运行时既有结构性凭据分类的一个稳定、不涉密的
投影：

- `configured`：凭据材料在结构上可用；
- `login_required`：该路由需要登录，或需要一个可用的登录能力；
- `missing`：API 式凭据不可用；
- `no_auth`：该路由显式禁用了凭据使用；
- `local`：该精确路由是本地且无需密钥的；
- `legacy`：该兼容路由无法被更精确地分类。

对于活动的具名自定义提供商，该状态是根据 `model_provider_id` 所指名的精确
路由算出的，而不是来自通用自定义提供商的默认值。
它有意把已保存密钥与已导入令牌的细节塌缩为
`configured`，把登录/同意来源的细节塌缩为 `login_required`。

响应从不包含端点 URL、凭据环境变量名、文件系统路径、凭据值、同意来源细节或令牌
元数据。`credentialState` 不是提供商金丝雀：`configured` 并不
证明端点可达、凭据有效、模型有权使用或请求会成功。下面那条模型路由也只是
一个选择目录；非空列表并不证明该路由当前能服务请求。

### `GET /v1/providers/{id}/models`

```json
{
  "provider": "deepseek",
  "models": [
    {
      "id": "deepseek-v4-flash-vision-exp",
      "image_input": "supported",
      "reasoning_effort": "unknown",
      "reasoning_effort_levels": [],
      "reasoning_effort_source": null
    }
  ]
}
```

对于一条精确的已配置路由，请提供 `?model_provider_id=vision-work`，
并要求响应回显同一个 `model_provider_id`。运行时会在读取模型支持能力之前，
在请求的提供商种类下解析该身份。
未知或不匹配的身份返回 `400`。具名分页游标会绑定
配置身份、端点与目录快照；改动其中任何一项
都需要重新开始分页。省略该查询参数会保留旧式目录
投影，并省略身份回显。

某个提供商的目录。未知 id 返回 `400`；旧式的 `deepseek-cn` 别名也返回 `400`，
因为它没有提供商元数据——请使用 `deepseek`。
空的 `models` 数组意味着运行时为该提供商没有可发现或已配置的
模型 id；它并不报告凭据是否存在。

这里返回的 id 正是 `POST /v1/threads` 的
`model` 字段与下面的 switch 路由所接受的值。`image_input` 是精确解析后的
提供商/模型路由的能力状态：`supported`、`unsupported` 或
`unknown`。请让 `unknown` 保持未知，而不要从模型名或
传输协议去推断。`supported` 描述的是模型路由；它并不意味着某个具体
客户端实现了图片上传控件。

`reasoning_effort` 使用同样的三种能力状态，描述
该精确模型的元数据是否公布了可选的思考强度阶梯。
`reasoning_effort_levels` 只包含来自该元数据的规范、被识别的活动强度等级。
Off 与诸如 none 之类的提供商同义词被排除：
Apps/Chat 协议把 off 当作省略，而这不证明支持一个
显式的提供商禁用命令。一个有能力推理的模型其活动强度阶梯仍可能
未知。
当原生兼容性会改变 Codex 等级的
线上值时（目前是 minimal 与 auto），它们也被排除。这个投影不改变原生
兼容行为，也不声明一个运行时无法原样发送的等级。
在自定义端点上，不会从整个提供商的默认值或一个熟悉的模型名
推断出任何等级。`reasoning_effort_source` 标识 `catalog`、
`codex_cli_cache` 或 `codex_app_server`；缺失、过期与无法识别的模型
元数据保持未知。Codex roster 元数据描述的是外部 CLI 的
roster，并不证明另一个单独配置的 Runtime 凭据属于
同一个账号，也不证明某个认证边界已被批准。

选择具名路由时请传入 `?model_provider_id=<exact configured id>`。
运行时会一并校验提供商种类与精确身份，在模型列表旁返回
`model_provider_id`，并保持活动
配置不变。请求的身份为空或未知，或种类不匹配，都会返回
`400`；它永不回退到另一个具名路由。

Runtime Chat 中继以 camelCase 发布同样的强度字段
（`reasoningEffort`、`reasoningEffortLevels`、`reasoningEffortSource`）。这些
模型事实不会启用工具执行，也不确立账号权益。

对于线程范围的选择，请把所选条目的提供商字段与所选模型
一同发出。当 `model_provider_id` 为 null 时请省略它：

```json
{
  "model_provider": "custom",
  "model_provider_id": "lm-studio",
  "model": "local-vision-model"
}
```

这会在那条精确的具名自定义路由上创建一个线程，而不改变
Runtime 的提供商或模型默认值。

### `PUT /v1/providers/{id}/key` —— 只写凭据

```json
// request
{ "key": "sk-…" }

// response
{ "provider": "openai-codex", "stored": true, "backend": "keychain",
  "credentialState": "configured", "configPath": "/…/config.toml" }
```

通过 `codewhale auth set --provider <id> --api-key-stdin` 所用的同一笔事务式写入
存储提供商 API 密钥：在提供商写锁下写入密钥存储，
加上持久化到配置文档并镜像进活动运行时
配置的 `[providers.<id>] auth_mode` 元数据标记，以便 `GET /v1/providers` 立即报告新状态。`backend`
说明哪个密钥后端持有该密钥，`configPath` 说明哪个配置
文档携带该标记（当环境配置是工作区范围时，就是用户全局文件）。

密钥永不被返回——没有读取凭据材料的
路由，且密钥及其长度都不出现在响应、错误或
日志中；响应只携带就绪度投影
（`credentialState`）。未知的提供商 id、`deepseek-cn` 旧式
别名、空密钥、超过 4 KiB 的密钥，或含控制字符的密钥
都返回 `400`。成功写入后得到 `credentialState: "local"` 对无密钥本地路由来说是诚实的
输出：密钥被存下了，但该路由被分类为不需要密钥。

### `DELETE /v1/providers/{id}/key` —— 清除 Codewhale 拥有的凭据

```json
// response
{ "provider": "openai", "cleared": true, "credentialState": "missing" }
```

通过 `codewhale auth clear` 所用的同一个共享所有者清除凭据：
配置文档会先做快照，若其保存失败则恢复，且只有在这次保存落地之后
才会去动密钥存储；被清除的标记会镜像进活动运行时配置，
使 `GET /v1/providers` 在下一次读取时就报告 `missing`，而不是等到
重启之后。

清除一条本就已清空的路由会返回 `cleared: true`——一个重试
撤销的客户端不应被告知哪里出了问题。如果配置条目
已被清空但密钥后端拒绝删除，该路由回答 `500`
并点名那个槽位：在密钥仍在钥匙串里时报告成功
会是对一次安全动作的谎报。

### 凭据所有权：`credentialSource` 与 `credentialWritable`

两个凭据动词都会拒绝一条其凭据不归 Codewhale 所有的路由，
而且 `GET /v1/providers` 携带同样的分类，以便客户端可以
在提交*之前*禁用其控件，而不是晚些时候才失败：

| `credentialSource` | `credentialWritable` | 含义 |
| --- | --- | --- |
| `secret_store` | `true` | Codewhale 自己的持久后端。唯一可写来源。 |
| `config` | `false` | 配置文件中的字面密钥，它在请求时仍然胜出。 |
| `external_auth` | `false` | 一个活动的对外同意（OAuth）拥有该凭据。 |
| `none` | `false` | 该路由不发送凭据，或没有凭据槽位。 |

当 `credentialWritable` 为 `false` 时，`credentialWritableReason` 携带
点名所有者的面向用户文案，且 `PUT` 与 `DELETE` 都以 `409`
与同一个理由作答。该分类是结构性的：它读取已声明的
认证模式、同意状态，以及任何已配置 `api_key` 值的*种类*，
并且永不解析密钥、环境变量值或认证命令。它是一个
类别，永不是一个值、一个路径或一个环境变量名。

### `POST /v1/providers/orcarouter/connect` —— 无终端的浏览器登录

```json
// 响应
{ "attemptId": "0f3c…", "authorizeUrl": "https://www.orcarouter.ai/auth?…",
  "expiresIn": 900 }
```

为没有终端的客户端运行与 `codewhale auth orcarouter` 相同的 OrcaRouter
OAuth 2.0 + PKCE 登录。Engine 绑定自己的回环回调
（`http://127.0.0.1:<port>/callback`），返回授权页 URL，并在后台最多等待
`expiresIn` 秒。客户端在系统浏览器（绝不是内嵌视图）中打开 `authorizeUrl`
并轮询该次尝试。浏览器回调后，Engine 交换授权码，拒绝授予 scope 不是 `api`
的 key，经与 CLI 相同的入口保存 key，再像 `PUT /v1/providers/{id}/key` 一样把
凭据标记同步到运行中的配置。它不会切换当前提供商；请用
`POST /v1/providers/{id}/switch`。

回调监听在 Engine 所在的计算机上，因此客户端只应在 Engine 与浏览器位于同一台
计算机时提供此路由。新的 `POST` 会取消上一次尝试，且只能读取当前尝试。拒绝情况：
id 不是 `orcarouter` 为 `400`；该路由凭据不可写为 `409`（理由与
`credentialWritableReason` 相同）；无法绑定回调为 `500`。`GET /v1/providers`
会在支持此路由的条目上标注 `"browserConnect": true`；否则以及在没有此路由的
Engine 上不出现该字段。

### `GET /v1/providers/orcarouter/connect/{attemptId}` 与 `DELETE …` —— 状态与取消

```json
{ "attemptId": "0f3c…", "status": "failed", "reason": "key_not_saved" }
```

`status` 为 `waiting`、`connected`、`declined`（用户在浏览器中拒绝）、
`expired`（未按时回调）、`cancelled` 或 `failed`。`failed` 附带 `reason`：
`authorization_error`（浏览器带回其他错误）、`callback_failed`、
`not_writable`（交换前该路由凭据已不可写；未创建 key）、`exchange_failed`、
`scope_refused` 或 `key_not_saved`。出现 `scope_refused` 或 `key_not_saved`
时，OrcaRouter 已创建了一个 Codewhale 未保存的 key，用户应在 OrcaRouter 的
Authorized Apps 中撤销它。`exchange_failed` 也可能发生在 OrcaRouter 已创建
key 但响应丢失之后。

`DELETE` 取消仍在等待浏览器的尝试。交换一旦开始，key 可能已经存在，因此该尝试
会继续，并报告 `connected`，或带 `key_not_saved` 的 `failed`；此时 `DELETE`
返回当前状态。未知或已被取代的 `attemptId` 为 `404`。只有 `POST` 响应携带授权页
URL；任何状态、错误或日志都不携带它、key、授权码或 verifier。

### `POST /v1/providers/{id}/switch`

```json
// request  (model is optional; omit to take the provider default)
{ "model": "qwen3.8-max" }

// response
{ "provider": "modelstudio-token-plan", "model": "qwen3.8-max",
  "message": "…", "persisted": true }
```

**请使用它，而不是用反复 `POST /v1/config` 写入加一次重载来模拟切换。**
提供商与模型在这里一起变动，改动会在被应用之前
针对提供商目录做校验，而 `persisted` 报告它是被写入配置还是
仅应用于活动会话。未知提供商 id 与 `deepseek-cn` 别名
以 `400` 被拒绝。

## 运行时数据模型

运行时使用持久的 Thread/Turn/Item 生命周期。

- **ThreadRecord** — `id`、`created_at`、`updated_at`、`model`、
  `model_provider`（通用种类）、`model_provider_id`（可选的精确已配置
  路由）、`workspace`、`mode`、`task_id`、`system_prompt`、`latest_turn_id`、
  `latest_response_bookmark`、`archived`
- **TurnRecord** — `id`、`thread_id`、`status`（`queued|in_progress|completed|
  failed|interrupted|canceled`）、`effective_provider`、`effective_model`、
  `effective_billing_surface`、时间戳、时长、用量、错误摘要、
  `artifacts` 与 `workspace`（参见 [回合工件](#回合工件)）
- **TurnItemRecord** — `id`、`turn_id`、`kind`（`user_message|agent_message|
  tool_call|file_change|command_execution|context_compaction|status|error`）、
  生命周期 `status`、`metadata`、`artifacts` 以及旧版的 `artifact_refs` 投影

事件是只追加的，带一个全局单调递增的 `seq` 用于回放/恢复。

`effective_billing_surface` 是从服务该回合的端点推导出的
非涉密分类。已识别的 StepFun 路由使用 `stepfun-payg` 或
`stepfun-plan`；未知与自定义端点则让它保持未设置。原始 base URL
不会被持久化到 `TurnRecord`。

### 重启语义

- 如果进程在某个回合或条目处于 `queued` 或 `in_progress` 时重启，
  恢复出来的记录会被标记为 `interrupted`，并带上 `"Interrupted by
  process restart"` 错误。
- 行尾换行符是事件追加的提交标记。启动时，一个不含该分隔符的尾部
  JSONL 片段会被截断并 fsync，即使它的字节构成合法 JSON；
  它是一次未提交的追加，其已预定的
  序列号不会被复用。以换行结尾的格式错误记录不是
  可识别的崩溃残片，在回放期间仍会失败关闭。
- 如果一个终态回合记录已落到磁盘但它的终态事件序列
  没有，第一次异步读取会把任何未解决的动态调用对账为
  `tool_call.canceled`，然后发出一个 `turn.completed`。既有的终态
  调用与回合回执会被识别出来，永不会重复。
- 回合操作绑定在重启后依然存在。用同一个 `operation_key`
  与请求重试会返回那个原始的已恢复回合（包括从中途进程退出恢复出的 `interrupted`
  回合）；不匹配的复用仍然是冲突。一个由崩溃产生、且从未取得回合的绑定会在
  启动时被丢弃，因为引擎提交只在两条记录都持久化之后才发生。
- 任务执行在同一套持久化的
  线程/回合存储之上执行自己的恢复。

### 审批模型

- `auto_approve` 标志作用于运行时审批桥与引擎
  工具上下文。当为某个线程/回合/任务启用时，需要审批的工具
  会在非交互式运行时路径中被自动批准，shell 安全检查
  以自动批准模式运行，且派生的子智能体继承该设置。
- 省略时，`auto_approve` 默认为 `false`。
- [授权顺序](AUTHORIZATION_ORDER.md)描述了带类型的规则、
  已注册的工具要求、安全底线、仓库法律、审批
  传输与沙箱执行之间的相对位置。

### SSE 事件流

`/v1/threads/{id}/events` 的 SSE 事件负载形态：

```json
{
  "schema_version": 1,
  "seq": 42,
  "previous_seq": 38,
  "event": "item.delta",
  "kind": "item.delta",
  "thread_id": "thr_1234abcd",
  "turn_id": "turn_5678efgh",
  "item_id": "item_90ab12cd",
  "timestamp": "2026-02-11T20:18:49.123Z",
  "created_at": "2026-02-11T20:18:49.123Z",
  "payload": {
    "delta": "partial output",
    "kind": "agent_message"
  }
}
```

兼容性说明：

- `schema_version` 是 HTTP/SSE 信封的 schema 版本。它与持久化的
  线程/回合/事件记录所用的运行时存储 schema 无关。
- 在既有客户端中 `event` 仍是 SSE 事件名；它被原样保留。
- `kind` 在面向带类型客户端的稳定信封中镜像 `event`。
- `seq` 在所有 Runtime 线程间全局分配。因此当其他线程交错时，
  同一线程事件之间出现间断是正常的。在这个逐线程 SSE 流上，
  `previous_seq` 是该线程上一个已送达事件的序列号（或第一个事件请求的回放
  游标）；客户端通过与它已接受的逐线程游标比较来检测丢失，
  而不是要求 `seq == previous_seq + 1`。一次追加被事务性回滚后序列分配也不会回卷，
  所以一次重试可以有意跳过一个未使用的值，
  而不意味着有事件丢失。
- `thread.started`、`turn.started` 与 `turn.completed` 仍与以前完全一样
  作为 SSE 事件名发出。
- 对 schema 版本 1 而言，`timestamp` 仍是规范的事件时间。`created_at`
  是给那些在其他地方使用 `created_at` 命名的客户端准备的等价别名；
  不要把两个字段都要求为存在。

### 结束与恢复线程流

每当服务器结束一个已经返回 `200` 的 `/v1/threads/{id}/events` 流时，最后一帧是
`stream.end`，恰好发送一次，而且总会发送（不需要 `progress=true`）：

```
event: stream.end
data: {"schema_version":1,"event":"stream.end","kind":"stream.end","thread_id":"thr_1234abcd","reason":"replay_failed","last_seq":42,"retryable":true}
```

- 它是传输帧，不是日志事件：它**没有 `seq`**，也**没有 SSE `id:`**。按 `seq` 确认的
  客户端会跳过它，浏览器的 `Last-Event-ID` 停留在最后一个真实事件上。
- `last_seq` 是流结束时的游标：在该连接上送达的最后一个日志 `seq`；如果一个都没有，
  则是实际的起始游标（在 `replay_limit` 尾部之后，它已经越过了被省略的历史）。它正是
  那个能无丢失、无重复地恢复的 `since_seq`。
- `retryable` 说明从 `last_seq` 恢复能否成功。客户端应以它为准，而不是以原因列表为准。
  对未知的 `reason`，按它的 `retryable` 处理。
- 没有自由文本消息。底层错误在 Runtime 日志里，其中可能包含存储路径。

| `reason` | 含义 | `retryable` |
| --- | --- | --- |
| `replay_failed` | 为开头回放提供数据的持久历史读取失败，包括第一个游标之后回放 worker 崩溃 | `true` |
| `catch_up_failed` | 广播落后之后，从流的游标开始的持久重读无法打开或失败 | `true` |
| `runtime_shutdown` | Runtime API 服务器正在停止（SIGINT、SIGTERM 或 SIGHUP；Windows 上为 Ctrl+C 或 Ctrl+Break）。打开的流会在进程退出前的一个有界排空窗口内收到此帧 | `true` |

每个 `200` 响应都带有 `x-codewhale-stream-end: 1`。有这个头时，**没有** `stream.end`
的 EOF 意味着连接或 Runtime 进程在服务器没有选择结束流的情况下死掉了：网络或代理
中断、崩溃，或不允许排空的终止（例如 `SIGKILL`）。早于此帧的 Runtime 不发送该头；
那时 EOF 仍然含义不明，应视为连接丢失。

客户端恢复规则：

1. 保持 `cursor` = 你接受的最后一个日志帧的 `seq`。忽略 `seq <= cursor` 的帧。
   如果某帧的 `previous_seq` 不是你的 `cursor`，说明你漏了事件：重新加载线程快照，
   而不是信任本地状态。
2. 收到 `retryable: true` 的 `stream.end` 时，在有界退避之后以 `since_seq = last_seq`
   重连，并告诉用户 Runtime 说了什么（例如“Runtime 正在关闭——正在重连”）。
   收到 `retryable: false` 时，停止，显示原因，并退回到快照。
3. 遇到没有 `stream.end` 的 EOF 或传输错误时，在有界退避之后以 `since_seq = cursor`
   重连，并显示为连接问题，而不是 Runtime 错误。
4. 流打开之前的 `401`/`403` 或 `404` 是终态（凭据或线程问题）。`5xx` 以退避重试。
5. 永远不要从 `0` 重连来“重新开始”：回放只有按游标才是幂等的。

Fleet 流（`/v1/fleet/runs/{run_id}/events`）保留自己的结束帧，
`fleet.stream.error {retryable}` 与 `fleet.replay.cursor_unavailable`。它们与
`stream.end` 不同：它们不携带游标，因为 Fleet 客户端从它接受的最后一个 Fleet 事件的
不透明 `cursor` 恢复。

### 引导送达

把一条引导放进引擎邮箱，与模型读到它是两回事。引擎会丢弃一条其回合已经推进的引导，
而被中断或失败的回合会丢掉它已排队的所有内容。API 报告的是
引擎的真实裁定，而不是那次尝试：

- 当引导被接受进邮箱时，条目被持久化为 `queued`。
- **已送达。** 引擎把文本提交进了该回合的记录：条目
  变为 `completed`，`steer_count` 上升，并发出 `turn.steered` + `item.completed`。
  `POST .../steer` 返回 `200` 与该回合。
- **未送达。** 该回合已经推进、被中断或先失败了：条目
  变为 `canceled`，`steer_count` 不上升，并发出 `turn.steer_dropped`，
  携带 `input`、`reason` 与已结算的 `item`。`POST .../steer`
  返回 `409`，因此客户端可以保留用户文本并重发，而不是
  因为一段从未被看到的引导而清空输入区。
- **仍待决。** 当引擎正处于一次长时间工具调用中时发出的引导
  无法在那次调用返回前结算，而请求不会为此挂住。
  短暂等待之后 `POST .../steer` 返回 `200`，条目仍为 `queued`；
  最终的 `turn.steered` 或 `turn.steer_dropped` 事件携带裁定。

因此，一个把 `200` 当作“模型看到了”的客户端在第三种情况下就是错的：
请读条目的状态，或等待那个事件。

常见事件名：`thread.started`、`thread.forked`、`turn.started`、
`turn.lifecycle`、`turn.usage`、`turn.steered`、`turn.steer_dropped`、`turn.interrupt_requested`、
`turn.completed`、`turn.artifacts`、`item.started`、`item.delta`、`item.completed`、
`item.failed`、`item.interrupted`、`approval.required`、`approval.decided`、
`approval.timeout`、`user_input.required`、`user_input.answered`、
`user_input.canceled`、`tool_call.requested`、`tool_call.resolved`、
`tool_call.timeout`、`tool_call.canceled`、`sandbox.denied`、
`turn.workspace_snapshot`、`turn.provider_request`、`runtime.store_failure`。

`turn.usage` 是**一次模型调用**的回执：父回合的模型调用报告用量时发出，
而不是每个回合一次。它的 `payload` 携带该次调用的 `usage`，
`duration_ms` / `first_token_ms` / `request_ms` 计时，`maxOutputTokens` 额度，
`billed_prompt_tokens`（提供商实际计费的提示大小：发生重试时取最后一次
请求的输入，否则取本次调用的输入数）、以及 `cost` —— 该次调用按它实际派发的
路由定价，形如 `{usd, cny, usd_priced, cny_priced}`。

当该路由无法为这次调用用到的所有 token 类别定价时，`cost` 为 `null`；
客户端不得把它读成“免费”。某币种的 `*_priced` 为 false 时，该币种未定价。
这些回执覆盖父回合的模型调用；子代理和压缩用量通过其他路径到达。
因此，逐调用价格之和只是部分估算，不是整个回合的完整账单。

`turn.completed` 的 `payload.turn.usage` 携带回合的权威累计 token 用量，
包括路由到其他工作的用量；它替换运行中的 token 计数，但不提供权威的
上一回合费用。`GET /v1/threads/{id}/usage` 的线程总量仍是记账依据。
移动页面在回合结束后保留已观察到的已定价调用估算，并标记为部分估算，
并在新回合开始时重置读数。重放的日志序号只计数一次。
`billed_prompt_tokens` 供 API 客户端使用；移动页头显示 token 用量和部分费用。

`turn.provider_request` 携带请求诊断观察，不增加计费用量；详见
[每次请求的缓存回执](CACHE.md#每次请求的回执)。

`runtime.store_failure` 是运行时报告操作者自己磁盘状态的故障：会话运行时
存储下的一个线程、回合或条目记录无法被读取、解析或写入。负载携带 `operation`
（`read` | `parse` | `write`）、`record_kind`（`thread` | `turn` | `item`）、
`record_id`、`path`、完整的 `error` 链、根因 `reason`、一个
`next_action`（该移开哪个文件，或到哪里检查可用空间与
权限），以及一行 `message`。当 `terminal` 为 `true` 时，该回合
自己的记录不可读或不可写，且不会再有 `turn.completed`；
等待该回合的客户端应把它当作失败。

智能体消息与推理增量会在其对应的 `item.delta` 事件被编序之前，物化进条目投影。
为避免为每个提供商碎片做一次 fsync，相邻增量在发布之前
被合并到配置的上限：最多 32 ms 或大约 16 KiB
（一个不可分割的上游块本身可能超过字节目标）。在该未发布窗口内发生进程崩溃会丢掉最近的尾部；
没有任何持久事件声称该尾部存在过。一旦 `item.delta` 变为持久，位于其
游标处或之后的快照就包含同一个已物化前缀。

当某条执行策略规则导致了这次提示时，`approval.required` 事件可能包含一个 `matched_rule` 字符串。
该字段是对客户端的解释性元数据，
不授予也不持久化权限。

`approval.required`、`approval.decided` 与 `approval.timeout` 携带两个
不同的标识符。`approval_id` 是 **审批** 一节所述、由 Runtime 铸造的一次性能力
——也是 `POST /v1/approvals/{id}` 唯一接受的値
——而 `approval.required` 还会把它重复在旧式的 `id` 字段中，供较旧
客户端使用。`tool_call_id` 是 Engine 的工具执行 ID，仅为
关联而存在。自动解决的提示（线程 `auto_approve`，以及从不
打开模态的 Auto-Review 姿态）也会铸造一个 `approval_id`，因此
该字段在每条路径上只有一种含义；那些 ID 不注册等待者，对
该端点也是惰性的。客户端绝不能把 `tool_call_id` 当作
审批能力，也不能假定它跨线程唯一。

线程事件流转发这些负载而不作改动。兼容回合
流携带 `approval_id`、它的 `id` 别名与 `tool_call_id`；待决
快照携带同一个能力与关联符，以便重连的客户端能把审批提示
挂到它的工具行上。

## 安全边界

- **默认仅回环。** 服务器默认绑定到 `127.0.0.1`。
  `--mobile` 也仅限回环，并在 TLS
  或经过验证的 overlay 传输边界存在之前拒绝非回环主机。运行时不提供
  用户隔离或 TLS。
- **可选令牌防护。** `--auth-token` 或 `DEEPSEEK_RUNTIME_TOKEN`
  要求 `/v1/*` 路由带上匹配的 bearer 令牌。这是一个本地
  便利防护，不能代替公网上的 TLS、VPN 或可信反向
  代理。
- **不代管提供商令牌。** 服务器永不返回 API 密钥。
  `api_key.source` 能力字段报告 `env`、`config` 或 `missing`——
  永不是密钥本身。
- **没有托管中转。** app-server 是一个由用户控制的本地进程。
  没有任何云组件。
- **能力响应**永不泄漏密钥、文件内容或会话
  消息正文。它们报告的是*元数据*：存在性、计数、状态标志。

### CORS 允许名单

运行时 API 自带一份内置的开发源站允许名单：
`http://localhost:3000`、`http://127.0.0.1:3000`、`http://localhost:1420`、
`http://127.0.0.1:1420`、`tauri://localhost`。要添加更多源站（例如
在 Vite 默认的 `:5173` 上开发 UI 时），可用以下任一方式：

- CLI 标志（可重复）：`codewhale serve --http --cors-origin http://localhost:5173`
- 环境变量（逗号分隔）：`DEEPSEEK_CORS_ORIGINS="http://localhost:5173,http://localhost:8080"`
- 配置（`~/.codewhale/config.toml`）：
  ```toml
  [runtime_api]
  cors_origins = ["http://localhost:5173"]
  ```

用户提供的源站会**堆叠在内置默认值之上**，而不是替换它们。
不支持通配符源站——显式允许名单模型被保留。跨源预检
只声明 `Authorization`、
`Content-Type`、`Accept`、`X-Codewhale-Runtime-Token` 以及兼容性的
`X-DeepSeek-Runtime-Token` 请求头；自定义请求头不被
允许。于 v0.8.10 加入（#561），于 v0.9.1 收紧（#4454）。

## 托管 Fleet 运行时与 SDK 助手

运行时 SDK 位于 `npm/runtime-sdk`，并作为
`@codewhale/runtime-sdk` 工作区包对外暴露。它有意保持轻薄：每个
助手都只是调用本地 Rust 运行时 API，因此无法绕过 Codewhale 的
沙箱、审批提示、提供商配置或 fleet 账本权威。

```js
import { createRuntimeClient } from "@codewhale/runtime-sdk";

const client = createRuntimeClient({
  baseUrl: "http://127.0.0.1:7878",
  token: process.env.CODEWHALE_RUNTIME_TOKEN,
});

const created = await client.createFleetRun({
  target: "this_computer",
  roles: [{ name: "reviewer" }, { name: "verifier" }],
  workflow: {
    id: "release-check",
    kind: "parallel",
    tasks: [
      { id: "review", name: "Review", instructions: "Review locally.", worker: { role: "reviewer" } },
      { id: "verify", name: "Verify", instructions: "Verify locally.", worker: { role: "verifier" } },
    ],
  },
});

// POST /runs only prepares durable work. This call crosses the launch gate.
await client.startFleetRun(created.run.id);

let cursor;
for await (const event of client.fleetEvents(created.run.id, { after: cursor })) {
  if (event.cursor) cursor = event.cursor;
  if (event.event === "fleet.replay.cursor_unavailable") {
    // Reload getFleetRun(created.run.id), then reconnect without the old cursor.
  }
}
```

托管路径刻意分为两步。`POST /v1/fleet/runs` 会校验并
持久化该运行与队列，但不启动 worker。另一个需认证的
`POST /start` 会激活它并调度执行器驱动；它的 `202` 响应
报告 `leased: 0`，因为驱动在拥有该运行之后才做所有租借。
创建要求具名角色、每个角色一个任务负责人、一个 `parallel`
工作流，以及一个显式的 Runtime 目标。v0.9.4 只执行
`this_computer`；`another_computer` 与 `cloud` 返回 `501`，而不是
静默在本地执行。worker ID 按运行生成；调用方指定的
`worker_specs` 返回 `501`，直到自定义 worker 可以被赋予无冲突的
托管身份为止。有效写入根重叠的并行任务会在运行被
记账（journal）之前被拒绝。托管的 `security_policy` 覆盖也会
失败关闭，直到那份文档能被端到端强制执行；可执行
权威来自每个具名角色的工具姿态与有界的任务工作区
范围。

Fleet 助手覆盖这套 HTTP 表面：

| 助手 | 运行时 API 路由 |
|---|---|
| `createFleetRun(spec)` | `POST /v1/fleet/runs` |
| `startFleetRun(runId)` | `POST /v1/fleet/runs/{run_id}/start` |
| `listFleetRuns()` | `GET /v1/fleet/runs` |
| `getFleetRun(runId)` | `GET /v1/fleet/runs/{run_id}` |
| `listFleetWorkers(runId)` | `GET /v1/fleet/runs/{run_id}/workers` |
| `getFleetWorker(workerId)` | `GET /v1/fleet/workers/{worker_id}` |
| `interruptWorker(workerId)` | `POST /v1/fleet/workers/{worker_id}/interrupt` |
| `stopWorker(workerId)` | `POST /v1/fleet/workers/{worker_id}/stop` |
| `restartWorker(workerId)` | `POST /v1/fleet/workers/{worker_id}/restart` |
| `stopFleetRun(runId)` | `POST /v1/fleet/runs/{run_id}/stop` |
| `replayFleetEvents(runId, options)` | `GET /v1/fleet/runs/{run_id}/events/replay` |
| `fleetEvents(runId, options)` | `GET /v1/fleet/runs/{run_id}/events`（SSE） |

`stopWorker` 会持久地取消该 worker 的活动任务，并让 Fleet 的其余部分继续运行。
`interruptWorker` 是同一个带尝试围栏的取消转换的兼容名称。
`stopFleetRun` 会取消每一个排队或活动任务，并把整个运行标记为已取消。

回放覆盖聚合的运行/任务转换与隐私有界的个别
worker 转换。事件正文省略提示词、工具调用 ID、完成文本、
工件路径/校验和以及取消身份；有界的失败理由
会经过密钥脱敏。`cursor` 是不透明的，在普通追加与 Runtime 重启之间保持稳定。
客户端用 `after=<cursor>` 重连。一次新请求会返回
有界的最新尾部，并在存在更早历史时标记 `history_truncated`。
账本压缩可能移除一个旧游标；那时 JSON 端点返回 `409`，
而 SSE 端点发出
`fleet.replay.cursor_unavailable`，因此客户端会重新加载当前运行
投影，而不是接受一个静默的空缺。

`GET /v1/runtime/info` 声明 `fleet_run_create`、`fleet_run_start`、
`fleet_event_replay`、`fleet_event_stream` 与 `fleet_local_target`。没有所请求路由的较旧
运行时仍会产生一个带类型的 SDK
`RuntimeCapabilityError`。

验证：

```bash
npm test --workspace @codewhale/runtime-sdk
```

## 智能体运行回执

子智能体通道把紧凑的运行回执持久化在
`.codewhale/state/subagents.v1.json`。运行时 API 把这些回执暴露为一个
只读检查表面：

| 操作 | 端点 |
|---|---|
| 列出已持久化的智能体运行 | `GET /v1/agent-runs` |
| 检查单个运行 | `GET /v1/agent-runs/{run_id}` |
| 停止单个运行 | `POST /v1/agent-runs/{run_id}/cancel` |

响应是与 `agent` 回执所呈现相同的 worker 记录形态：
`spec.run_id`、`actor_kind`、生命周期 `status`、有界的 `events`、
`follow_up`、`takeover`、`artifacts`、`usage` 与 `verification`。对于较旧的记录，`run_id`
回退为 worker id，且 `{run_id}` 可以是
运行 id 或 worker id。

这些端点不启动也不引导子智能体。这个 API 表面存在的目的是让
app/编辑器/无头客户端可以检查 TUI 与
父模型看到的同一批交接回执，并停止一个它们正在展示的运行。

`POST /v1/agent-runs/{run_id}/cancel` 不接受请求体。它通过与 TUI 的停止及 `agent/cancel` 工具相同的会话范围路径
停止该运行：后代随它一起停止，且写入范围内的子智能体被改动的文件会在
其结果中被点名，而不是被丢掉。它用 worker 记录作答：

- 当记录已是终态时返回 `200`（停止一个已完成的运行是
  空操作，返回它的回执）；
- 当拥有它的引擎已接受停止、但在数秒内尚未记录终态
  回执时返回 `202`；请轮询 `GET /v1/agent-runs/{run_id}`；
- 未知运行时返回 `404`；
- 当该运行属于本运行时并未托管的会话时返回 `409`（例如
  一个单独的终端会话）；请从那个会话停止它。

## 会话生命周期（原生 UI 监督）

| 操作 | 端点 |
|---|---|
| 列出会话 | `GET /v1/sessions` |
| 列出会话摘要 | `GET /v1/sessions/summary` |
| 获取会话 | `GET /v1/sessions/{id}` |
| 重命名 / 归档会话 | `PATCH /v1/sessions/{id}` |
| 删除会话 | `DELETE /v1/sessions/{id}` |
| 会话存储修复摘要 | `GET /v1/sessions/repair` |
| 恢复为线程 | `POST /v1/sessions/{id}/resume-thread` |
| 创建线程 | `POST /v1/threads` |
| 列出线程 | `GET /v1/threads` |
| 附着到事件 | `GET /v1/threads/{id}/events?since_seq=0` |
| 发送消息 | `POST /v1/threads/{id}/turns` |
| 引导 | `POST /v1/threads/{id}/turns/{turn_id}/steer` |
| 中断 | `POST /v1/threads/{id}/turns/{turn_id}/interrupt` |
| 压缩 | `POST /v1/threads/{id}/compact` |

## 兼容性测试

契约快照位于 `crates/protocol/tests/`。请运行：

```bash
cargo test -p codewhale-protocol --test parity_protocol --locked
```

这会校验 app-server 的事件 schema 没有偏离已记录的契约。
CI 在每次推送到 `main` 时以及发布标签上都会运行它。

app-server 的 stdio 控制表面有自己的漂移防护——所声明的
`capabilities` 方法集合被钉在 `crates/app-server/src/tests.rs`：

```bash
cargo test -p codewhale-app-server capabilities
```

发布之前，请运行无头冒烟（stdio 探针 + 可选的提供商
矩阵，不泄漏密钥）：

```bash
scripts/release/app-server-smoke.sh --matrix        # dry-run plan
bash scripts/release/app-server-smoke.test.sh       # parser self-test (fake binary)
```


## 账号宪章快照

Runtime 能力 `profile_constitution` 允许 `POST /v1/threads/{id}/turns` 携带
`profile_constitution: {accountId, revision, constitution}`。`constitution` 严格为
`{schemaVersion: 1, detail, initiative, collaboration, notes}`，选项分别是
`brief|balanced|detailed`、`check|judgment|moving`、`direct|critical|coach`；
`notes` 最多 4,000 个 Unicode 字符。无效数据被拒绝。

已认证的账号传输提供快照，快照参与回合重放身份。Engine 使用现有个人宪章渲染器，
记录到原生会话历史；提供商重试、压缩、内部续接与 RLM 子调用保持已接纳的快照，
不会中途重读宿主操作者的账号。新快照完整替换旧个人偏好；权限与审批策略不变。

没有随请求提供快照时，Engine 在回合接纳时从配置的账号服务读取已登录资料。
Engine 自行读取的资料不可用或无效时，本回合改用未登录的本地宪章，并在本会话中只提示一次；宿主随请求
提交的 `profile_constitution` 快照无效时仍会使回合失败，`GET /v1/constitution` 仍会报告该错误。
没有已保存偏好的账号使用显式默认
快照，未登录账号使用现有本地宪章。托管传输始终提交所属账号快照（包括默认值），
以免本地偏好跨账号泄露。

`GET /v1/constitution` 读取下一回合的资料与模型指导，不是活跃回合已采用新编辑
的回执。`POST /v1/constitution/preview` 接受宪章文档，返回
`{modelGuidance, saved:false}`，不保存。`modelGuidance` 是 Engine 渲染的指导正文；
同一文档的 GET 与预览返回相同文本，不含会话记录中的来源行或封装。
`source` 与 `revision` 保留来源信息。两条路由遵循普通 Runtime 授权；
旧 Runtime 必须升级后才能接收携带账号宪章的回合。
