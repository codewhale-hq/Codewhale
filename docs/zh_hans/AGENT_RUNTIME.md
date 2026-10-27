# Codewhale 智能体运行时：一个持久的底座，多种熟悉的启动方式

> 英文原文：[AGENT_RUNTIME.md](../AGENT_RUNTIME.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

本文说明子智能体（sub-agent）、无头 `exec` 路径、Agent Fleet 和 Runtime 之间的关系。这些概念一度漂移成了*两套*并行的 "worker" 体系。修复方式是让 **Runtime worker run（Runtime 的 worker 运行）** 成为持久的执行原语：Fleet 负责智能体的身份、成员和选择；Runtime 负责执行、授权和生命周期。"子智能体"作为嵌套角色的产品用语仍然有用，但不能因此暗示存在一个生命周期语义更弱的独立执行底座。本文也回答了 #2972 中悬而未决的方向问题（"与 Claude Code 收敛到什么程度才合适？"）。

## 核心思想

在后台（detached）运行智能体工作的东西**只有一个**：一个带有持久执行生命周期的**无头 Runtime worker**。它是一个模型循环，拥有完整的、受授权把关的工具面，并且可以通过同一套生命周期继续委派子工作。其他一切都只是选择、启动或观察这一个 Runtime 的方式。

```
                 ┌──────────────────────────────────────┐
                 │          headless Runtime             │
                 │ execution · authority · lifecycle     │
                 │       can spawn child workers         │
                 └───────────────┬──────────────────────┘
                                 │
                 ┌───────────────┴──────────────────────┐
                 │      one durable execution substrate  │
                 └───────┬───────────────┬──────────────┘
                         │ launches      │ launches
          ┌──────────────┴──────┐  ┌─────┴────────────────┐  ┌──────────────────────┐
          │       TUI turn       │  │   `codewhale exec`  │  │      Agent fleet        │
          │ interactive, in-proc │  │    headless CLI     │  │ identity · membership │
          │                      │  │ full tools · stream │  │     · selection       │
          └─────────────────────┘  └──────────────────────┘  └──────────┬───────────┘
                                                                          │
                                                                          └─ selects a Runtime worker
```

- **子智能体**是面向用户的名称，指带有角色的*嵌套任务*（`explore`、`reviewer`、`implement`、`test` 等，规范的七种角色见 `docs/SUBAGENTS.md`）。它应当由与 Fleet 所选智能体相同的 Runtime worker 生命周期支撑。`agent` 是面向模型的启动器，不是第二个运行时。
- **`codewhale exec`** 是无头入口：任何人在任何时候都可以使用（CI、脚本、另一个智能体），拥有完整工具，输出 `stream-json` 事件流，并且可以派生子智能体。它就是带 CLI 的那个运行时。
- **Fleet 选中的智能体**以 Runtime 的一次 `codewhale exec` 运行来执行。Fleet 提供身份、成员和选择，不会重新实现执行。持久账本、调度/租约/重试、授权、本地或 SSH 传输，以及终态生命周期，都由 Runtime 负责。

所以 "Fleet 还是子智能体" 并不是在两种执行底座之间二选一。Fleet 回答**谁**有资格并被选中，Runtime 回答已授权的工作**如何、在哪里**执行，子智能体则仍是嵌套任务的角色/UX 用语。

## 切换规则

如果一个后台（detached）的 `agent` 子级会因一次性的提供商（provider）超时而失败且不重试，而等价的 Runtime worker 会重试并保留账本证据，那么切换就没有完成。应把这视为 Codewhale Runtime 的缺口，而不是正常的 "子智能体行为"。

兼容性的 `agent` 运行时现在会对提供商响应头、流和超时这类瞬时失败做带退避的重试，之后才把 worker 标记为中断；重试耗尽时会保留检查点并返回一个续接句柄。剩下的收敛工作，是让这套生命周期在进程重启、远程执行和完整的 Runtime 账本调度下保持持久。

目标规则是：

- 持久的或长时间运行的工作走 Runtime worker 生命周期；
- `agent` 应当把工作入队给 Runtime worker 运行或观察它，而不是自己拥有独立的生命周期；
- 进程内子级只允许作为小范围的兼容性/延迟优化，并且必须暴露与持久 Runtime 路径相同的终态、重试语义、回执和检视句柄。

在产品语言里，说 "打开一个子智能体" 没问题。在架构语言里，这意味着 "以此角色启动一个嵌套的 Runtime worker"，也可以选用 Fleet 选出的成员。

## 为什么是这种形态（以及为什么它能解决卡顿）

起因是：派生大量进程内子智能体会让 TUI 卡顿，因为每个子级都会克隆一个沉重的运行时并重建整个工具注册表，*而且* TUI 会为每个子级渲染完整的卡片/对话记录（transcript）。

调研 Claude Code、Codex 和 Kimi 之后发现，让编排器在高扇出下保持轻量的，**不是**进程边界——三者都在进程内运行子智能体。真正起作用的是**隔离 + 紧凑的事件流**：

- 子级的对话记录**绝不**回流到父级——父级只拿到结果摘要和一条小的生命周期事件流；
- UI 渲染的是**计数**（`2 running / 3 done`），而不是每个 worker 一个子会话；
- 每个 worker 的工具面直接根据**角色/能力配置**构建，而不是"先全部构建再过滤"。

因此，"无头" 的意思是*执行不再以 UI 为形状*，**并不**意味着能力变少。无头 worker 保留完整工具集，并且可以派生子智能体。

当工作还需要**持久**（TUI 关闭、笔记本休眠后仍能继续）或**远程**（SSH）时，Runtime 会把 worker 作为 `codewhale exec` 在进程外运行。Fleet 可以提供被选中的智能体身份，但执行权和生命周期归属仍在 Runtime。这样，沉重的构造完全放在另一个进程里，编排器无论扇出多大都保持流畅，运行也能在重启后存活——这就是 #3154 的以天计的自主运行目标。

## 单一递归轴

worker 从 `spawn_depth = 0` 起运行，只要满足 `spawn_depth + 1 ≤ max_spawn_depth` 就可以派生子级，所以预算 `N` 提供 `N` 层嵌套委派。子智能体和 Fleet 选中的 Runtime worker 共用**同一条**轴，其来源是 `codewhale_config`：

Runtime 委派默认不设置产品深度上限。操作者显式设置的深度仍约束后代，子级不能扩大权限。0 禁止子级委派，但根 worker 仍可运行。内部 `u32` 表示会拒绝溢出，它不是建议的工作预算。

面向模型的 `agent` schema 不提供深度控制。兼容字段 `max_depth`、`maxDepth` 和 `max_spawn_depth` 仍可解析，但模型请求不能用它们给子级设置上限；回执会注明被忽略的字段。Exact Fleet 和保存的 Runtime 权限仍然生效。

Workflow IR 结构与子级委派分开。其 `max_depth` 默认 0（不设上限），正数操作者配置会被遵守。解析器及 VM 的栈、内存边界仍在执行前校验文档。

## 事件词汇

Runtime 执行账本持久化的是 worker 自己的事件流，而不是另一套模拟出来的分类。兼容性 API 和类型仍以 `Fleet...` 前缀暴露它。`codewhale exec --output-format stream-json` 会输出 `{"type": "content" | "tool_use" | "tool_result" | "agent_spawned" | "sandbox_denied" | "workflow_event" | "session_capture" | "service_released" | "turn_usage" | "metadata" | "status" | "request_image_window" | "done" | "error"}` 形式的行，它们映射到 Runtime 账本的兼容类型 `FleetWorkerEventPayload`（`RunningTool`、`WorkflowEvent`、`Running`、`Completed`、`Failed` 等）。`workflow_event` 在 Workflow 运行期间携带带类型的 run/phase/task/gate 回执，并作为带类型的 `WorkflowEvent` 保留在 Runtime 执行账本中；终态的 `done` 或 `error` 仍由外层 Runtime worker 负责。一套词汇，两个表面。

`session_capture` 在 exec 运行把自己的对话记录保存为会话时发出一次，并且只在一个位置携带可恢复的 id：

```json
{"type": "session_capture", "schema": "codewhale.exec-stream", "schema_version": 1,
 "content": "<redacted:…>", "saved_session_id": "01J…"}
```

- `saved_session_id` 是原始的已保存会话 id，只在保存成功之后才发出。对本地 Fleet worker，父级会分配一个新 ID，并共用 Runtime 现有的会话目录。只有当报告的恰好是这个 ID、且对应的已保存对话记录可以加载时，执行器才会公布 `FleetReceipt.saved_session_id`。拥有 Runtime API 访问权限的客户端随后可以通过 `GET /v1/sessions/{id}` 读取回复。SSH worker 保留其摘录和远程日志，但不会公布一个不可用的本地会话链接。id 只是查找键，不能代替 Runtime 的身份认证。
- `content` 是与终态 `metadata.session_id` 相同的脱敏指纹，所以单独截获的 `metadata` 回执仍可安全写入日志，两个事件之间也仍能关联。因此 `metadata.resume_command` 指向的是这个字段（`codewhale exec --resume <session_capture.saved_session_id>`），而不是自己携带 id。

终态 `metadata` 回执还携带 worker 可见的最终回答：`visible_final_answer_chars` 是最终助手回复的真实字符数，`visible_final_answer_excerpt` 是它的摘录，有长度上限（4,000 个字符，被截断时以 `...` 结尾），并已脱敏；当前回合没有产生可见回答时，该字段省略。恢复的回合绝不会复用旧的回复，失败或中断的回执可能携带当前回合的部分文本；以回执状态为准。Runtime 执行器从这个回执读取摘录——绝不从流式 `content` 增量读取，那些是运行过程中的"边想边说"——并把它附加到 `Completed.summary`；对于没有评分器、也没有文件产物的任务，还会把它作为该任务的交付物写入回执备注。生命周期事件标签和 worker 检视摘要只显示一小段摘录；事件 `payload` 和回执保留完整摘录。

`turn_usage` 是每次模型调用的用量回执：当提供商报告了该次调用的用量时，每个模型请求（回合内的步骤）发出一次：

```json
{"type": "turn_usage", "schema": "codewhale.exec-stream", "schema_version": 1,
 "turn": 1, "input_tokens": 1200, "output_tokens": 180,
 "reasoning_tokens": 90, "prompt_cache_hit_tokens": 900,
 "prompt_cache_miss_tokens": 300, "prompt_cache_write_tokens": 0,
 "reasoning_replay_tokens": 40, "duration_ms": 1834}
```

- `turn` 是这次 exec 运行内该模型调用的序号，从 1 开始；`input_tokens`、`output_tokens` 和 `duration_ms` 始终存在。
- 提供商没有报告的可选 token 字段会被**省略**——绝不输出为 null，也绝不用 0 回填。字段名与终态 `metadata` 回执保持一致：`prompt_cache_hit_tokens` 是提供商的缓存读取计数（Anthropic 的 `cache_read_input_tokens`），`prompt_cache_write_tokens` 是缓存创建计数（`cache_creation_input_tokens`）。`reasoning_tokens` 只出现在会报告它的提供商路径上（OpenAI 兼容的 `completion_tokens_details` / Responses 的 `output_tokens_details`；Anthropic 不报告思考 token 计数）。`reasoning_replay_tokens` 是对 DeepSeek V4 交错思考（interleaved-thinking）重放的客户端估算值。
- `finish_reason` 是该步骤提供商报告的停止原因（例如 `stop`、`tool_calls` 或 `length`），提供商未报告时省略。
- 当提供商对某次调用完全没有报告用量时，该次调用的整个事件都会被跳过。做延迟/收敛分析时，应当对 `turn_usage` 事件求和，而不是根据墙钟时间去推断每一步的 token；终态 `metadata` 回执仍然携带累计总数。

## 与 Claude Code 的收敛（#2972）

Codewhale 应当在**形态**上与 Claude Code 收敛，而不是在品牌上：

- **采纳**：带有真正的 CLI/SDK 入口的无头运行时；作为隔离运行、返回摘要（而非对话记录）的子智能体；紧凑的、事件驱动的扇出投影；能力/角色工具配置；技能生态（#2743）；结构化的运行回执。
- **保持不同**：Codewhale 品牌，以及对 DeepSeek/GLM/MiniMax 和多提供商的一等支持；本地优先的 **Agent fleet**，作为身份、成员和选择层；由 Runtime 负责的持久本地/SSH 执行和授权；以 Workflow 作为排序覆盖层。
- **不要**按表面分叉执行语义。TUI、`agent`、`exec` 和 Runtime API 都必须驱动*同一个* Runtime，并观察*同一条*事件流。Fleet 的选择会传给这个 Runtime，而不是另建一条执行路径——正是这里的分歧造成了"两个移动靶"，本文档的存在就是为了防止它。

检验任何新智能体表面的试金石是：*它是启动并观察那唯一的运行时，还是另造了一个？* 只有前者被允许。

## 历史说明：v0.9.0 之后还剩什么

已归档的路线图快照——实时状态以 issue 跟踪器为准，而不是这份列表。2026-08-17 根据对较早的 0.9 时代文档的全面审计刷新。这些计划是证据，不是第二个真相来源。v0.9.0 整合了水下 shell（underwater shell）、消息优先的 Operate、权限姿态（permission postures）、已接通的 Workflow 引擎和持久运行日志、Lane CLI/运行时、带 `operate_ready` 的设置流程、宪章（constitution）再平衡，以及 ProviderLake/Models.dev。剩余工作属于后续版本：

1. **品牌重塑收尾**：`deepseek`/`deepseek-tui` 二进制 shim 及其 shim 发布资源已在 v0.9.0 移除；剩下的义务是 Homebrew `codewhale` formula 的推出（`docs/REBRAND.md`）。
2. **把 Operate 做成价值流**：在水下 shell 之上做一个控制面板表面（WIP、队列年龄、瓶颈）；阶段历史（#4039）；以 Workrooms Phase 2（#3209/#3210）作为收件箱底座；回执对账。
3. **流量控制**：真正的 WIP 上限和可见的队列（#4015、#4016），与已发布的 16 并发/1k 运行访问模型（#4292）协调一致。
4. **Fleet 身份与 Runtime/Workflow 收敛的遗留项**：实时 tmux/verifier-gate 自用验证，以关闭 #4175/#4177/#4178/#4179；Fleet 使用规范的 AgentProfiles 并选择成员，而由 Runtime 负责执行；Conductor/topology（#4010、#4012）作为延伸目标。
5. **TTC 设计实现**（设计文档在 `codewhale-ops` 中）：已批准，v0.9.0 之后不再受阻。
6. **HarnessProfile 收尾**：状态/UX 展示线（`docs/rfcs/HARNESS_PROFILE_CUTLINE.md`）。
7. **文件拆分，已落地**：v0.9.0 时代的超大文件已拆开：`main.rs` 现在只是一个薄桩，`ui.rs` 已拆分成 `crates/tui/src/tui/` 下职责集中的模块（如今约 3.9k 行；`docs/rfcs/FILE_DECOMPOSITION_0_9_0.md` 中的数字是 0.9.0 时代的快照）。剩下的工作是 `POST_0_9_1_SEAMS.md` 中跟踪的"核心之上的薄 TUI"这一北极星目标。

这些文档自己明确推迟的事项：外部工作流记忆（仅定边界）、自动 harness 演化、托管 workroom、`constitution_modules`（需要签字确认）、权限配置（#3211，需要设计），以及 plan 上限探测（需要产品决策）。

## 外部 harness 的公开启动契约（#4641）

外部评测 harness（例如未来 Verifiers v1 的内置 harness）通过启动公开的 `codewhale exec` 入口来嵌入 Codewhale，并让它指向自己拥有的拦截端点。Codewhale 只拥有自己的**启动契约**；拦截、轨迹、模型调用计时、token 计量、重试、rollout 限制和运行时编排都归 harness。不要往 Codewhale 里添加 harness 运行时、轨迹解析器或回执 schema。

可复现的无头启动只使用现有的通用接口：

- 一份显式的临时配置，写明路由和凭据的**环境变量**，绝不写密钥本身：

  ```toml
  provider = "openai"

  [providers.openai]
  base_url = ""            # the harness fills in its interception endpoint
  model = ""               # the harness fills in the target model
  api_key_env = "VF_CODEWHALE_API_KEY"
  ```

  （`base_url` 由 harness 填入它的拦截端点；`model` 由 harness 填入目标模型。）

- `CODEWHALE_HOME` 设为每次运行全新的目录；
- `CODEWHALE_SECRET_BACKEND=file`；
- `CODEWHALE_MCP_CONFIG` 指向一个为每次运行生成的 MCP JSON 文件，其中只包含 harness 提供的任务服务器（`{"mcpServers":{"task-tools":{"url":""}}}`；`mcpServers` 别名以及基于 URL 的 Streamable HTTP / SSE 传输已经存在）；
- `CODEWHALE_MEMORY=false` 和 `CODEWHALE_TELEMETRY=false`。0.9.12 的源码默认开启用量计数，并提供退出开关。每个封闭的 harness 都要显式设置这个运行级的关闭开关，这样测试就不会从全新或复用的 home 中采集或发送数据。普通的已启用会话会把聚合计数发送到一个端点（`https://telemetry.codewhale.net/v1/telemetry`，即随发行版附带的默认值），而不是本地文件。这是一条硬底线：环境变量中显式的 "off" 优先于 `--telemetry true` 和配置里的 `telemetry = true`。如果 harness 希望已启用的 home 继续在本地缓冲、而不联系任何地方，请改为设置 `CODEWHALE_TELEMETRY_ENDPOINT=`（留空）。参见 [`docs/TELEMETRY.md`](./TELEMETRY.md)；
- 仅当 harness 提供受信任的 `http://` 拦截端点时，才设置 `CODEWHALE_ALLOW_INSECURE_HTTP=1`（容器/隧道端点并不总是回环地址）；
- 调用方提供时，再加上 `--append-system-prompt` 和 `--disallowed-tools`。

拦截密钥只留在子进程环境中（通过路由的 `api_key_env` 解析）；它绝不会被写入 argv、路由配置、日志、`stream-json` 流或任何生成的文件。

确切的参数顺序如下：

```sh
codewhale \
  --config .vf-codewhale/config.toml \
  --workspace . \
  --no-project-config \
  --skip-onboarding \
  exec \
  --auto \
  --sandbox danger-full-access \
  --output-format stream-json \
  -- "<task prompt>"
```

`--no-project-config` 必须出现在子命令**之前**（和 `--skip-onboarding` 一样）。公开的分发器会解析它，并把它转发到 TUI 子命令之前；随后 `Exec` 会跳过按工作区区分的 `[workspace]`/`[projects]` 用户配置叠加层，使配置面只取决于显式的 `--config`。`crates/tui/tests/integration/verifiers_harness_contract.rs` 是这份契约的、不依赖提供商的验收锁。

### 未来的上游清单（不在本文范围内，不要执行）

真正把 Codewhale 加为内置 harness 的工作在外部的 Verifiers 仓库里进行；它所需要的、带校验和清单的公开且不可变的 Codewhale GitHub Releases，自 v0.9.1 起就已存在（最新已发布版本是 v0.9.13，发布于 2026-09-14；工作区源码版本是 0.9.13）。预计这项上游改动仅限于一个新的 `verifiers/v1/harnesses/codewhale/` 包，以及它的测试矩阵和文档注册：其中 `CodewhaleHarnessConfig` 固定目标发布版本，`setup()` 下载并校验已发布的归档，`launch()` 写入上文的临时路由/MCP 文件并调用 `runtime.run_program(...)`。

明确**不**由这项契约工作完成的遗留事项：打标签、发布或创建 Codewhale release；打开或提交上游 Verifiers PR；运行其需要凭据的 E2E 矩阵；以及在确切的已发布归档尚未在该上游运行时里跑过之前，宣称对该运行时/架构的支持。
