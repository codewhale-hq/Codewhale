# 旧版 `.deepseek/` 兼容路径 —— 审计与迁移状态（#3068）

> 英文原文：[LEGACY_PATHS.md](../LEGACY_PATHS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

## v0.9.10 清理台账

这是发布窗口内针对残留物的决定记录，这些残留物在文本搜索里否则可能看起来
彼此可以互换。一条兼容条目必须指出仍然需要它的契约；
新的生产代码必须使用当前路径。

| 类别 | 对象 | 决定 / 契约 |
|---|---|---|
| DELETE | 已退役的 underwater 构建版本测试覆盖项 | 已在 `1cc4d1d1b` 中移除；所属测试此前已经删除，仅用于生产的构建版本仍是权威。 |
| MIGRATE | 跨任务 Agent Mail | `bd998495a` 与托管应用 `4ff7bc50d` 用一个可持久化的类型化运行时（runtime）协议，取代了靠猜的事件别名和同会话邮箱混用。 |
| MIGRATE | TUI `status_message` 写入 | 活跃残留；被触碰到的流程必须迁移到类型化的 toast/当前状态所有者。不允许新增写入。 |
| COMPATIBILITY | `.deepseek/` 状态与已存储的提供商（provider）别名 | 为升级过的安装和已存储的提供商身份提供读取回退；写入仍只走 Codewhale。详情见下。 |
| COMPATIBILITY | `agent_message` 转录（transcript）/协议解码 | 已持久化的转录和工具线上取值需要解码；它不是跨任务 Agent Mail 传输。 |
| COMPATIBILITY | 托管运行时信封 schema 1 | 用于此前持久化/登记过的 runner 重放；当前本地 Codewhale Agent Mail 生产者是 schema 2，两者都会归一化为托管信封。 |
| CURRENT | DeepSeek 提供商支持 | 一个真实可选的提供商，不是产品品牌；按提供商划分的名称与配置保留。 |
| CURRENT | 同会话子智能体（subagent）邮箱 | Start/status/peek/message/followup/interrupt/wait/cancel 的作用域仍限于父级运行时会话。 |
| CURRENT | Web 语言字典 | 应用与文档路由没有页面本地的 `isZh` 分支；新文案继续走字典。 |
| CURRENT | 安全、授权、协议、持久化、迁移与数据完整性测试 | 它们保护外部或可持久化的契约，不是文案/布局清理的候选对象。 |

2026-08-19 的快照：`crates/tui` 下有 `10,954` 个 Rust `#[test]` 属性，
`523` 行 TUI `status_message` 源码，配套托管应用候选版里 `apps/web/app` 下的
页面本地 `isZh` 文件为零。计数是盘点信号，不是删除目标。

Codewhale 是从 DeepSeek-TUI 更名而来的。为了避免破坏已有安装，运行时会从新的
`~/.codewhale/` 位置读取状态，但**回退**到旧版的 `~/.deepseek/` 位置，
并且始终**写入** `~/.codewhale/`。本文审计每一处旧版引用，并记录
保留 / 弃用 / 移除的决定，使迁移可审计。

## 规范解析器（新代码请用它）

状态目录 API 由 `codewhale-config` 导出，实现在
`crates/config/src/state_dirs.rs`；根目录选择由 `crates/paths/src/lib.rs` 负责。
本文路径表示默认值；`CODEWHALE_HOME` 可以选择主目录，并禁止回退到该根目录之外。

| 符号 | 用途 |
|---|---|
| `CODEWHALE_APP_DIR` / `LEGACY_APP_DIR` | `codewhale-paths` 定义的默认目录名。 |
| `codewhale_home()` | 选定的主目录，默认是 `~/.codewhale`。 |
| `legacy_deepseek_home()` | 选定的旧版主目录，默认是 `~/.deepseek`。 |
| `resolve_state_dir(subdir)` | 读取主目录；仅在该目录不存在且未显式选择主目录时回退。 |
| `ensure_state_dir(subdir)` | 在主目录下写入；受保护的迁移可以搬移产品自有的旧版状态，并报告变化。 |

兼容其他工具的根目录只是只读发现输入，不是产品自有的迁移或删除目标。

## 逐路径决定

**下文所有旧版引用的决定：保留为回退。** 移除 `.deepseek` 回退会让那些
就地升级、从未重新跑过引导的用户陷入困境。只有在某个发布版于首次运行时
主动把 `~/.deepseek/` 迁移为 `~/.codewhale/`，并经过一个弃用窗口之后，
才重新审视此事。

| 引用 | 是否经由 `resolve_state_dir` 路由？ | 决定 |
|---|---|---|
| `config::resolve_state_dir` / `ensure_state_dir` | 不适用（解析器本身） | 保留 —— 规范 |
| `crates/skills/src/roots.rs`（`~/.deepseek/skills`） | 否 —— 兼容根目录枚举 | 保留只读发现；这不是产品自有的写入目标。 |
| `crates/engine/src/prompts.rs`（`LEGACY_HANDOFF_RELATIVE_PATH = ".deepseek/handoff.md"`） | 否 —— 显式旧版常量 | 保留 —— 显式的旧版交接回退 |
| `crates/tui/src/workspace_trust.rs` | 否 —— 硬编码 | 保留为回退；后续处理 |
| `crates/engine/src/session_manager.rs` | 否 —— 硬编码 | 保留为回退；后续处理 |
| `crates/runtime/src/skill_state.rs` | 否 —— 硬编码 | 保留为回退；后续处理 |
| `crates/engine/src/tools/skill.rs` | 否 —— 硬编码 | 保留为回退；后续处理 |
| `crates/snapshot/src/lib.rs` | 否 —— 硬编码 | 保留为回退；后续处理 |
| `crates/runtime/src/workspace_discovery.rs` | 否 —— 硬编码 | 保留为回退；后续处理 |

## 后续源代码改动

逐路径表保留原始审计记录，并修正已搬移的代码归属。迁移前核对当前调用方，
复用现有议题和解析器，保留显式主目录隔离、读取回退、所有权及恢复保护。
使用现有的针对性检查验证。本文不授权搬移或删除其他工具的根目录。
