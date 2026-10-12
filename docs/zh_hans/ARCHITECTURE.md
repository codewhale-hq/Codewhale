# Codewhale 架构

> 英文原文：[ARCHITECTURE.md](../ARCHITECTURE.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。
> 2026-10-06 补齐单一回合循环的私有阶段与 ACP 投影边界。

Codewhale Engine 是现有 Rust 执行运行时。[Runtime API](RUNTIME_API.md) 是客户端
接口，[TypeScript mods](EXTENSIONS.md) 提供经审查的扩展。

当前边界说明（工作区版本以 `Cargo.toml` 为准；该边界自 v0.9.1 起保持不变）：
- `crates/tui` 仍是 TUI、运行时 API、任务管理器和工具执行循环的现行终端用户运行时。
- 其他工作区 crate 正在逐步拆出，但它们还不是唯一的权威运行时。
- `crates/tui` 正按照 `docs/design/TUI_DECONSTRUCTION.md` 记录的顺序拆分为以智能体
  工作分区为单位的 crate：契约、工具库、服务、提供方、运行时、叶子功能 crate、TUI 视图
  与宿主。TUI 始终是唯一写终端的 crate。在某个模块迁走之前，
  它仍位于 `crates/tui/src` 下的原路径。
- LSP 子系统（`crates/tui/src/lsp/`）已完整接入引擎的工具执行后路径
  （`core/engine/lsp_hooks.rs`），在 `File` 写入、编辑和补丁动作之后提供内联诊断。
- swarm 智能体（agent）系统已在 v0.8.5 移除。当前生效的子智能体（sub-agent）接口面
  是单一的 `agent` 工具；持久化 RLM 会话可通过延迟加载的 `rlm` 动作族使用。
  现行代码库中不再保留任何模型可见的 swarm 工具。

## 高层概览

```
┌─────────────────────────────────────────────────────────────────┐
│                         User Interface                          │
│  ┌─────────────────┐  ┌─────────────────┐  ┌────────────────┐  │
│  │   TUI (ratatui) │  │  One-shot Mode  │  │  Config/CLI    │  │
│  └────────┬────────┘  └────────┬────────┘  └────────┬───────┘  │
└───────────┼─────────────────────┼────────────────────┼──────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Core Engine                              │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │                    Agent Loop (core/engine.rs)           │   │
│  │  ┌─────────┐  ┌─────────────┐  ┌──────────────────────┐ │   │
│  │  │ Session │  │ Turn Mgmt   │  │ Tool Orchestration   │ │   │
│  │  └─────────┘  └─────────────┘  └──────────────────────┘ │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Tool & Extension Layer                      │
│  ┌──────────┐  ┌──────────┐  ┌─────────┐  ┌────────────────┐   │
│  │  Tools   │  │  Skills  │  │  Hooks  │  │  MCP Servers   │   │
│  │ (shell,  │  │ (plugins)│  │ (pre/   │  │  (external)    │   │
│  │  file)   │  │          │  │  post)  │  │                │   │
│  └──────────┘  └──────────┘  └─────────┘  └────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
            │                     │                    │
            ▼                     ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                  Runtime API + Task Management                  │
│  ┌─────────────────────────────┐  ┌──────────────────────────┐  │
│  │ HTTP/SSE Runtime API        │  │ Persistent Task Manager  │  │
│  │ (runtime_api.rs)            │  │ (task_manager.rs)        │  │
│  └─────────────────────────────┘  └──────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
            │                     │
            ▼                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                        LLM Layer                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │               LLM Client Layer (client.rs)               │  │
│  │  ┌──────────────────┐  ┌─────────────────────────────┐   │  │
│  │  │ OpenAI-compatible │  │  Anthropic / Responses      │   │  │
│  │  │  (chat adapter)  │  │   (adapters)                │   │  │
│  │  └──────────────────┘  └─────────────────────────────┘   │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## 模块组织

### 入口点

- **`crates/cli/src/main.rs`** - 唯一可执行程序的入口。`crates/cli/src/lib.rs` 拥有命令接口；终端和无头运行时通过 `crates/tui/src/lib.rs` 中的 `codewhale_tui` 库在同一进程内启动。

### 核心组件

- **`core/`** - 主要引擎组件
  - `engine.rs` - 引擎状态、操作处理、消息处理
  - `engine/turn_loop.rs` - 现有 Engine 外层回合循环、共享工具规划/执行/结果处理与
    流解码。私有 `turn_loop/` 阶段包括准备（`preparation.rs`）、模型派发与接纳
    （`model_step.rs`）、有序续接（`continuation.rs`）、内联 REPL 编排
    （`inline_repl.rs`）及直接模型工具批次（`tool_batch.rs`）。它们借用同一个
    Engine 与 TurnContext，不引入第二个运行时、会话、提示词、审批、事件或持久化
    权威。重试、循环终止与立即返回仍不同，只有现有有效工作路径推进步骤。
  - `session.rs` - 会话状态管理
  - `turn.rs` - 基于回合的对话处理
  - `events.rs` - 用于 UI 更新的事件系统
  - `ops.rs` - 核心操作

### 配置

- **`config.rs`** - 配置加载、profile、环境变量
- **`settings.rs`** - 运行时设置管理

### 工作区 crate

- **`crates/cli`** - 唯一的 `codewhale` 可执行程序及命令接口。它自身拥有
  `auth`、`metrics`、`update` 等命令，并通过 `codewhale_tui::run(RuntimeOptions, args)`
  在同一进程内启动终端和无头模式（`run`、`exec`、`doctor`、`sessions`……）。
  `crates/tui` 是库；`codew` 和旧发布文件名的别名包含相同的可执行程序。
- **`crates/tools`** - 共享的工具调用原语，包括 TUI 运行时使用的工具结果/错误/能力类型。
- **`crates/agent`** - 模型/提供商（provider）注册表（ModelRegistry），用于把模型 ID
  解析到提供商端点。
- **`crates/app-server`** - 用于无头智能体工作流的 HTTP/SSE + JSON-RPC 应用服务器
  传输层。唯一的可执行程序将 `app-server --http`/`--mobile` 在同一进程内
  路由到由 `codewhale_tui` 库承载的运行时 API。
- **`crates/config`** - 配置加载、profile、环境变量优先级、CLI 运行时覆盖。
- **`crates/cloud-facts`** - 拉取已签名的 Codewhale 云端事实信道（`facts/v1`），
  校验其 Ed25519 信封，并维护一份已验证的磁盘缓存；从不是启动依赖。
- **`crates/command-contract`** - 为分阶段抽离 TUI 命令而设的命令能力与分发形态
  原型；仅是形态，还不是生产分发路径。
- **`crates/core`** - 提供商中立的请求构造（`request.rs`）、有界上下文片段、
  工具调用解析器，以及线程/会话类型。它**不**拥有智能体循环：现行回合循环是
  `crates/tui/src/core/engine/turn_loop.rs` 里的 `Engine::run_turn`，而
  `crates/tui/src/core/` 是 TUI crate 内部的模块，不是这个 crate 的。这里曾有一棵
  占位的 `engine/` 目录树让人误解——它没有任何调用方，还会在不接触模型的情况下
  发出 `TurnComplete`——已在 v0.9.11 移除。源码守卫跟踪已解析的本地阶段调用，
  仍拒绝未登记的循环 owner。ACP stdio 投影现有 Runtime manager 与 Engine，
  不保留独立提供商/工具回合循环或历史。
  递归 RLM 和普通 Python RPC 现在使用同一个 Engine 生产者与 Session；RLM
  不再有独立循环。Python 保存上下文与变量，每轮只借用调用方已捕获的路由、
  Native 选择、原有代码审批、取消信号和截止时间。任务指导有界且追加到 Core
  策略；递归历史完整保留，超过预算时拒绝而不压缩。持久 `rlm` 上下文只属于
  调用方会话，`share_session=true` 明确拒绝。子智能体也以已捕获的准入事实
  使用同一个 Engine；两种嵌套宿主都不再保留独立循环例外。
- **`crates/execpolicy`** - 用于工具执行决策的审批（approval）/沙箱（sandbox）策略引擎。
- **`crates/hooks`** - 响应、工具、作业和审批生命周期事件的事件接收端（sink：stdout、
  JSONL 文件、webhook、Unix socket），外加可选启用的 lifecycle outbox。
  用户在工具调用前后运行命令的自定义 shell 钩子（hook）是 `crates/tui/src/hooks.rs`
  里的另一套系统。
- **`crates/localization`** - 面向用户的 UI 界面字符串的语言环境注册表
  （`crates/localization/locales/*.json`）；它从不改变提示词（prompt）或模型
  输出语言。
- **`crates/mcp`** - 用于 Model Context Protocol 工具服务器的 MCP 客户端 + stdio 服务器。
- **`crates/memory`** - 本地、带作用域、带来源信息的记忆（memory）与可恢复状态
  （是一个库，不是第二个智能体循环）。
- **`crates/models`** - 提供商的请求/响应模型，以及离线模型元数据目录。
- **`crates/palette`** - 终端 UI 的颜色 token、主题和对比度计算。它的 `ratatui`
  feature（默认开启）门控所有渲染相关代码；主题 id、设置规范化和十六进制解析
  在不开该 feature 时也能编译，运行时就是这样链接它的。
- **`crates/paths`** - 用户作用域的运行时路径权威（`CODEWHALE_HOME` 与平台 home 解析）。
- **`crates/protocol`** - 请求/响应分帧与协议类型。
- **`crates/runtime`** - `codewhale-runtime`，正在从 `crates/tui` 拆出的无头运行时
  （`docs/design/TUI_DECONSTRUCTION.md`）。目前它承载最先迁走的叶子模块（重试状态、
  安全标签、休眠守卫、会话树……）以及 `host_terminal`——运行时代码通过这唯一的
  端口向终端 UI 索取终端效果。它从不依赖 TUI、`ratatui` 或 `crossterm`；
  `scripts/check-command-crate-boundaries.py` 强制这一点，并对 `crates/tui` 中
  残留的 runtime -> UI 引用做棘轮式收敛。
- **`crates/secrets`** - API key 存储用的 OS 密钥环集成，外加 UI 与运行时代码共用的
  输出净化器（`sanitize`）和脱敏（`redact`）。
- **`crates/state`** - SQLite 线程/会话持久化层。
- **`crates/telemetry`** - 匿名、用户可关闭的聚合使用计数；唯一被允许构建或发送
  遥测载荷的 crate（`docs/TELEMETRY.md`）。
- **`crates/workflow`** / **`crates/workflow-js`** - 工作流（workflow）引擎及其
  QuickJS 脚本层（由 whaleflow 系列 crate 更名而来）。
- **`crates/lane`** - Lane 运行时：Fleet/Workflow 工作的持久化、可挂接运行实例
  （`codewhale lane list/status/attach/logs/stop`）。
- **`crates/release`** / **`crates/build-support`** - 发布检查与构建链路。

### LLM 集成

- **`client.rs`** - 现行 HTTP 客户端层：OpenAI 兼容、Anthropic 和 Responses
  线格式适配器、DeepSeek 请求边界处理、重试策略和流式传输。提供商路由经共享的
  配置与目录层落到这里。
- **`llm_client/`** - LLM 客户端 trait、重试逻辑和错误分类（`LlmClient`、
  `RetryConfig`、`with_retry`），由 `client.rs` 使用；`mock.rs` 仅用于测试
  （`#[cfg(test)]`）。
- **`crates/models`**（`codewhale_models`）- API 请求/响应的数据结构；
  TUI crate 没有本地的 `models.rs`。

#### DeepSeek API 端点

DeepSeek 暴露 OpenAI 兼容端点。第一方路由使用：
- `https://api.deepseek.com/beta` - 默认的 DeepSeek base URL（`provider_defaults.rs`）
- `https://api.deepseek.com/beta/models` - 实时模型发现与健康检查

为了兼容 OpenAI SDK，也接受 `https://api.deepseek.com/v1`，并且仍可显式配置它，
以退出仅有 beta 提供的功能，例如严格工具模式、聊天前缀补全和 FIM 补全。
DeepSeek 的公开文档并未记录这条工作流可用的 Responses API 路径；引擎通过
Chat Completions 驱动回合。

### 工具系统

- **`tools/`** - 内置工具实现
  - `mod.rs` - 工具注册表与通用类型
  - `shell.rs` - shell 命令执行
  - `file.rs` - 文件读写操作
  - `todo.rs` - 清单工具以及遗留的 todo 别名
  - `tasks.rs` - 模型可见的持久化任务、门禁、后台 shell 和 PR 尝试工具
  - `git.rs` - 只读的 `git_status` / `git_diff` 检查包装
  - `git_tool.rs` - 规范化的基于动作的 `Git` 工具（`status | diff | log | show | blame`）；按动作划分的遗留别名已在 v0.9.3 移除
  - `git_history.rs` - 只读的 `git_log` / `git_show` / `git_blame`
  - `github/` - 统一的 `github` 工具族（只读上下文，加上由 `gh` 支撑的受控
    评论/关闭动作）；默认延迟加载，可通过 `tool_search` 发现
  - `automation.rs` - 基于 `AutomationManager` 的模型可见调度工具
  - `plan.rs` - 规划工具
  - `subagent/` - 子智能体启动与监督。`agent` 是唯一的创建接口面；
    `subagent/coord.rs` 在既有管理器之上补上一组窄口径协调工具（`agents/list`、
    `agents/message`、`agents/followup`、`agents/interrupt`、`agents/wait`、
    `agents/coordinate`）。`agent_open`/`agent_eval`/`agent_close` 生命周期接口面
    已退役（见 `subagent/coord.rs` 模块文档）
  - `spec.rs` - 工具规格
  - `rlm.rs` - 持久化的递归语言模型（RLM）会话——持久的本地 Python REPL 子进程
    （清理过环境变量，但没有操作系统级沙箱），支持语义化辅助调用和 `var_handle` 输出

### 扩展系统

- **`mcp.rs`** - 面向外部工具服务器的 Model Context Protocol 客户端
- **`skills/`** - 针对本地 `SKILL.md` 文件的技能（skill）发现与注册表，外加安装与审计
- **`hooks.rs`** - 带条件的执行前/后钩子

### 用户界面

- **`tui/`** - 终端 UI 组件（基于 ratatui；这是代表性列表，并非穷尽——该模块
  已增长到 80 多个专一职责的文件）：
  - `app.rs` - 应用状态与消息处理
  - `ui.rs` - 事件处理、流式状态与渲染逻辑
  - `approval.rs` - 工具审批对话框
  - `clipboard.rs` - 剪贴板处理
  - `underwater.rs` - 主 shell 界面：状态标签（chip）、模式标签、阶段导轨

### LSP 集成

- **`lsp/`** - 编辑后诊断注入（#136）
  - `mod.rs` - `LspManager` ——按语言惰性创建的传输池 + 配置
  - `client.rs` - `StdioLspTransport` ——基于 stdio 的 JSON-RPC，支持 `didOpen`/`didChange`/`publishDiagnostics`
  - `diagnostics.rs` - 诊断类型、严重级别和 HTML 块渲染器
  - `registry.rs` - 语言检测与默认服务器映射：`rust-analyzer`、
    `gopls`、`pyright-langserver`、`typescript-language-server`、`jdtls`、
    `intelephense`（PHP）、`vue-language-server`、`clangd`（`lsp/registry.rs:98-110`）
  - 通过 `core/engine/lsp_hooks.rs` 接入引擎——每次成功编辑后调用

### 安全

- **`sandbox/`** - 平台沙箱策略准备与拒绝上报
  - `mod.rs` - 沙箱类型定义
  - `backend.rs` - 可插拔的沙箱后端抽象（把 shell 执行路由到远程服务，
    例如 Alibaba OpenSandbox）
  - `policy.rs` - 沙箱策略配置
  - `opensandbox.rs` - Alibaba OpenSandbox HTTP 后端适配器
  - `seatbelt.rs` - macOS Seatbelt 配置生成
  - `bwrap.rs` - Linux bubblewrap 命令包装器
  - `seccomp.rs` - 休眠中的 Linux seccomp 实现；未接入命令执行
  - `process_hardening.rs` - 针对 TUI 进程自身的 Linux 内核级加固
    （纵深防御；不是子命令沙箱）
  - `windows.rs` - Windows 辅助程序契约；在存在 Job Object 进程围栏辅助程序
    之前不予宣称

### 实用工具

- **`utils.rs`** - 通用工具
- **`logging.rs`** - 日志基础设施
- **`compaction.rs`** - 长对话的上下文压缩
- **`purge.rs`** - 智能体驱动的上下文清除（精确移除/改写个别消息）
- **`pricing.rs`** - 成本估算
- **`prompts.rs`** - 系统提示词模板
- **`runtime_api.rs`** - HTTP/SSE 运行时 API（`codewhale serve --http`）
- **`runtime_threads.rs`** - 持久化线程/回合/条目存储 + 可回放的事件时间线
- **`task_manager.rs`** - 持久化队列、worker 池、任务时间线和产物（artifact）

## 数据流

### 交互式会话

1. TUI 接收用户输入
2. 输入由 `core/engine.rs` 处理
3. 消息通过 `client.rs` 发送给 LLM
4. 响应流式返回，在 `client.rs` 中解析
5. 提取工具调用并通过 `tools/` 执行
6. 工具执行前后触发钩子
7. 结果聚合后送回 LLM
8. 最终响应在 TUI 中渲染

### 崩溃恢复 + 离线队列

1. 发送用户输入之前，TUI 会把检查点（checkpoint）快照写入 `~/.codewhale/sessions/checkpoints/latest.json`
2. 启动默认从新会话开始；此前的会话通过 `--resume`/`--continue`（或 TUI 里的 `Ctrl+R`）显式恢复
3. 降级/离线期间，新的提示词在内存中排队，并镜像到 `~/.codewhale/sessions/checkpoints/offline_queue.json`
4. 队列编辑（`/queue ...`）持续持久化，草稿和已排队的提示词可跨重启保留
5. 回合成功完成后清除当前检查点，并写入一份持久会话快照
6. 具备动作能力的回合还会在 `~/.codewhale/snapshots/<project_hash>/<worktree_hash>/.git` 下生成回合前/后的 side-git 工作区快照；`/restore N` 和 `revert_turn` 恢复文件状态，但不改动对话历史或用户的 `.git`

### 工具执行

1. LLM 通过 `tool_use` 内容块请求工具
2. 工具注册表查找处理器
3. 执行前钩子运行
4. 当生效的权限姿态（posture）与策略要求时，请求审批
5. 执行工具（在 macOS 上可能被 Seatbelt 包装，在 Linux 上可能被 bubblewrap 包装）
6. 执行后钩子运行
7. 结果元数据保留在运行时条目记录上
8. **LSP 编辑后钩子**：在 `File` 的写入、编辑或补丁动作之后（包括仅用于回放的遗留别名），当 LSP 启用时，引擎会运行 `run_post_edit_lsp_hook()` 以收集诊断
9. **诊断刷写**：在下一次 API 请求之前，`flush_pending_lsp_diagnostics()` 会把已收集的错误作为一条合成用户消息注入
10. 结果返回给智能体循环

### 后台任务

1. 客户端入队任务（`/task add ...` 或 `POST /v1/tasks`）
2. `task_manager.rs` 在 `~/.codewhale/tasks` 下持久化任务 + 队列条目
3. 有界 worker 池中的 worker 领取排队任务，状态转为 `running`
4. 任务创建/使用一个运行时线程，并启动一个运行时回合
5. `runtime_threads.rs` 持久化线程/回合/条目记录 + 单调递增的事件序列
6. 时间线/工具摘要/产物引用增量持久化
7. 清单状态、验证器门禁、PR 尝试和受控的 GitHub 事件，从工具元数据应用到当前任务
8. 最终状态（`completed|failed|canceled`）是持久的，可通过 TUI/API 查询

模型可见的持久化任务工具是同一个管理器之上的一个接口面。它们不引入并行的工作
体系：`task_create` 入队普通任务，`checklist_*` 更新任务本地进度，`task_gate_run`
和已完成的 `task_shell_wait` 附加验证证据，自动化运行也入队普通的持久化任务。

### 运行时线程/回合时间线

1. API/TUI 创建或恢复线程（`/v1/threads*`）
2. 在线程上启动回合（`/v1/threads/{id}/turns`）
3. 引擎事件被映射为条目生命周期事件（`item.started|item.delta|item.completed`）
4. 中断/引导操作只作用于当前回合
5. 压缩（自动/手动）以 `context_compaction` 条目生命周期形式发出
6. 清除（智能体驱动）以 `context_purge` 条目生命周期形式发出
7. 客户端回放历史，并用 `/v1/threads/{id}/events?since_seq=<n>` 续接

### 持久化 schema 门禁

- `session_manager.rs`、`runtime_threads.rs` 和 `task_manager.rs` 在持久化记录中内嵌 `schema_version`。
- 加载时，若 schema 版本更新则显式报错拒绝，而不是静默截断/覆盖数据。
- 这样既能安全地向前迁移，也能在二进制与存储状态不同步时防止损坏。

## 扩展点

### 新增一个工具

1. 在 `tools/` 中创建处理器
2. 在 `tools/registry.rs` 中注册
3. 添加工具规格（名称、描述、输入 schema）

### 新增一个 MCP 服务器

1. 在 `~/.codewhale/mcp.json` 中配置
2. 启动时自动发现服务器
3. 工具自动暴露给 LLM

### 创建一个技能

1. 创建带 `SKILL.md` 的技能目录
2. 定义技能提示词和可选脚本
3. 放入 Codewhale 拥有的根目录（`~/.codewhale/skills/` 或
   `<workspace>/.codewhale/skills/`），或通过 `/skills` 从兼容的 harness 根目录导入

关于技能管理器、审计清单，以及“兼容根目录（`.claude`、`.agents` 等）绝不被原地
修改”这条规则，见 [SKILLS.md](SKILLS.md)。

### 新增钩子

在 `~/.codewhale/config.toml` 中配置：

```toml
[[hooks]]
event = "tool_call_before"
command = "echo 'Running tool: $TOOL_NAME'"
```

## 关键设计决策

1. **流式优先**：所有 LLM 响应都流式返回，以保证响应速度
2. **工具安全**：Ask 和 Auto-Review 会依据工具与托管策略要求审批；Full Access
   去掉常规提示，但不会去掉硬性安全闸。有副作用的 MCP 工具走同一条边界。
3. **可扩展性**：MCP、技能和钩子让定制无需改动代码
4. **跨平台**：核心可在 Linux/macOS/Windows 上工作。沙箱保证因平台而异：
   macOS 在可用时使用 Seatbelt；Linux 在探测证明可用时默认使用已安装的 bubblewrap
   可执行文件；Windows 没有对外宣称的操作系统命令沙箱。Seccomp 和 Windows
   辅助程序契约未接入命令执行。
5. **最小依赖**：为构建速度谨慎选择依赖
6. **本地优先的运行时 API**：HTTP/SSE 端点面向受信任的 localhost 访问，
   目前由 `crates/tui` 运行时提供
7. **锁中毒**：默认失败即停。锁中毒意味着某个持有者在改到一半时 panic，
   因此标准做法是 `.expect()` 并附上指明该锁的消息——绝不对外提供只更新了
   一半的状态。只有在状态过期是安全的场景（缓存、幂等重建）才用 `into_inner()`
   恢复，并加注释说明原因。

## 配置文件

- `~/.codewhale/config.toml` - 主配置（`~/.deepseek/config.toml` 仍作为遗留回退被读取）
- `/etc/deepseek/managed_config.toml` - 可选的托管默认值层（Unix）
- `/etc/deepseek/requirements.toml` - 可选的允许策略约束（Unix）
- `~/.codewhale/mcp.json` - MCP 服务器配置
- `~/.codewhale/skills/` - 用户技能目录
- `~/.codewhale/sessions/` - 会话历史
- `~/.codewhale/sessions/checkpoints/` - 崩溃检查点 + 离线队列持久化
- `~/.codewhale/snapshots/` - 供 `/restore` 和 `revert_turn` 使用的 side-git 回合前/后工作区快照
- `~/.codewhale/tasks/` - 后台任务记录、队列、时间线、产物
- `~/.codewhale/audit.log` - 仅追加的安全事件：凭据的保存与清除、钩子环境变量的键名、压缩过程、目标完成、终端的审批路由、Auto-Review 裁决，以及开启 `[network]` 审计时的出站网络决定。它不是操作记录：不包含命令或文件改动，app 或 `serve` 回合也不会在这里写入审批。一个会话做了什么，见 `docs/RECEIPTS.md`
- `~/.codewhale/sessions/<id>/approval_receipts.jsonl` - 一个会话的每一次审批请求与决定，包括由谁决定
