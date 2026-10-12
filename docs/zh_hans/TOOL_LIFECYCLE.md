# 历史工具面生命周期策略（v0.8.53）

> 本文翻译自英文版 [TOOL_LIFECYCLE.md](../TOOL_LIFECYCLE.md)，与英文修订同步（英文正文自 `6b31b2001` 起仅新增了指向本文的语言链接）。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

**状态：** 本文是历史设计记录，不是当前运行时文档。v0.9.1 的规范动作面和仅回放别名契约记录在 [`RUNTIME_SIMPLIFICATION_DESIGN.md`](../RUNTIME_SIMPLIFICATION_DESIGN.md) 和 [`TOOL_SURFACE.md`](../TOOL_SURFACE.md) 中。本旧周期没有任何目录代码落地——相关代码工作**已推迟**。本文档是 GitHub **#2681** 的总纲政策，**#2682** 和 **#2683** 是该计划中"瘦身"的具体实例。它描述的是*将要做什么*，以及未来任何瘦身 PR 必须保持的不变量。

**相关未完成工作的范围（请勿与之矛盾）：**
- PR **#2684** —— 子智能体角色词汇、生命周期信号、评估可用性。本政策中的旧子智能体命名清理与护栏测试将在 #2684 之上重新设定基线。
- PR **#2685** —— git 历史激活 + RLM/字段错误。

**实际发生的情况（这样你可以把其余部分当作历史来读）：** 下面的"隐藏兼容"计划*并未*完整落地。`exec_wait` 和 `exec_interact` 被移除而非保持可分发。工作进度面与此不同：`todo_write` 是唯一的模型可见名称，而 `work_update`、`TodoWrite`、`todo`、`checklist_write`、`checklist_update` 仍注册为 `TodoWriteTool` 的隐藏兼容别名，以便旧转录可以回放；`checklist_add`、`checklist_list`、`todo_add`、`todo_update`、`todo_list` 未注册且不可调用。`tts`/`speech` 仍可分发。已落地的契约见 [`TOOL_SURFACE.md`](../TOOL_SURFACE.md)。请把 §4 和 §8 当作一份被否决的目录瘦身提案来读，而不是保证列出的每个别名都已消失。

**本文中所有 file:line 引用在 v0.8.52/0.8.53 时是正确的，现在已过期。** 它们没有被重写，因为给历史记录重新编号会使其看起来像是当前的。本文引用但已完全不存在的符号包括 `ARCEE_FIRST_TURN_NATIVE_TOOLS` 和 `apply_provider_tool_policy`（两者都被 `1bfcced43c` "fix(engine): remove Arcee tool catalog exception" 移除），以及计划中的 `HIDDEN_COMPATIBILITY_TOOLS` / `DEPRECATED_ALIASES` 集合（从未写入）。在依据本文行动之前，请对照代码树核实这里的任何符号。

---

## 1. 目的与弱模型问题

Codewhale 自带庞大的原生工具面。该面的首回合 *active* 分区是每个模型在运行任何一次 `tool_search_*` 调用之前都能看到的全部内容。如今这个 active 集合里包含若干**近乎重复的工具**，它们以不同名称映射到*同一个*实现：

- `exec_wait` 和 `exec_shell_wait` 都是 `ShellWaitTool`（`crates/tui/src/tools/registry.rs:526,529`）。
- `exec_interact` 和 `exec_shell_interact` 都是 `ShellInteractTool`（`registry.rs:527,530`）。
- `tts` 和 `speech` 都是 `SpeechTool`（`registry.rs:787-792`，两者均被延迟）。
- `todo_write` 是唯一的模型可见 `TodoWriteTool` 面；`todo_write`、`TodoWrite`、`todo`、`checklist_write`、`checklist_update` 是它的隐藏兼容别名。

对强模型而言，冗余名称是无害的噪声。但对**较弱的 / 较小的模型**（Arcee Trinity 车道、`deepseek-v4-flash` 子执行器以及任何非思考型执行器）来说，可见集合中每多一个近似重复都是真实的成本：

- 它用*毫无区别*的选项拓宽了选择空间，增加了工具误选和在同义词之间摇摆的概率。
- 它把稀缺的首回合目录预算（第 5 节）花在零信息条目上。
- 它稀释了"一个名称 = 一件事"的契约，而这个契约正是小模型理解整个工具面的基础。

生命周期政策的存在就是为了**收缩并约束模型可见的工具面**，同时绝不破坏回放引用已退役名称的旧转录的能力。

### v0.9.1 的规范工作跟踪面

模型可见的进度面是单一工具：`todo_write`（#4132）。智能体和 Fleet 中的 worker 用它来记录活动运行时线程或持久任务下的具体 To-do / 工作进度。

`task_*` 和 Fleet/Workflow 账本仍是持久的生命周期所有者。清单元数据是进度的模型可见投影：`task_updates.checklist` 携带当前条目、完成百分比和进行中的条目。

**To-do 是唯一的工作面。** `update_plan` 是对话式推理——面向复杂项目的策略、上下文和路线说明。它不是进度面，不得重复 To-do 条目，仅计划状态永远不会渲染为 To-do 快照。

**任何请求都不会重述清单。** 模型了解 To-do 上有哪些内容的方式与其他任何信息相同：通过它自己的 `todo_write` 调用——调用的输入就是完整清单——以及该调用返回的简短回执（计数和进行中的条目）。两者都是普通的持久化历史。不会向父回合循环步骤或子智能体步骤追加任何内容。完整清单始终在 UI 中可见，而 UI 是与请求不同的另一个面。

`crates/tui/src/todo_snapshot.rs` 渲染那唯一的受限主体——在条目数和字符数上都硬受限，进行中的条目被优先保留，任何省略都被标记——用于三个*一次性*显示快照的接缝，因为是人提出的要求：新分叉的智能体拿到的 `<codewhale:fork_state>` 块、`/relay` 以及转录内的智能体卡片。

该渲染器的两个属性是承重的：

- **权威来源。** 当运行时拥有该清单时，快照从 `WorkRuntime` 图投影读取，因为 `todo_write` 在那里暂存，之后才发布到旧的 `SharedTodoList` 视图。没有附加运行时的会话直接读取清单。
- **每智能体隔离。** 每个智能体读取*它自己的*清单（#4810），因此 worker 看到的是自己的进度，绝不会看到父智能体或兄弟智能体的。父智能体的清单只作为不可变的 `<codewhale:fork_state>` To-do 段到达分叉出的子智能体，在分叉接缝处解析，因此同回合的 `todo_write` 会被包含在内。

渲染器约束并框定快照；它不审核 To-do 内容。它保证条目文本不能提前闭合包装器、不能用控制字符伪造行格式、不能超出条目/字符上限——但并不保证任意条目文本可以安全地作为指令来遵循。

`todo_write` 注册的隐藏兼容别名是 `work_update`、`TodoWrite`、`todo`、`checklist_write`、`checklist_update`（`ToolRegistryBuilder::with_todo_tool`）。它们仍然针对同一 To-do 状态可分发，因此旧转录可以在不丢失数据的情况下回放，但不会向模型目录通告（`TodoWriteTool::model_visible` 仅对规范名称为 true）。`checklist_add`、`checklist_list`、`todo_add`、`todo_update`、`todo_list` 未注册且不可调用。

---

## 2. 五种生命周期状态

每个原生工具名称恰好占据一种生命周期状态。

| 状态 | 含义 | 首回合可见？ | 在 `tool_search_*` 中？ | 调用即执行？ | 何时使用 |
|---|---|---|---|---|---|
| **active** | 规范的，位于首回合目录头部 | **是** | 不适用（已在 active 中） | 是 | 模型默认应使用的工具 |
| **deferred** | 已注册 + 可发现，按需加载 | 否 | **是** | 是 | 真实有用的工具，但不值得首回合槽位 |
| **hidden-compatibility** | 已注册 + 可分发，但从 active **和**搜索中移除 | 否 | **否** | **是 —— 行为完全相同，静默** | 仅为了让旧转录能回放而保留的旧同义词；不应有模型重新发现它 |
| **deprecated** | 类似 hidden-compat，但执行时会**向结果元数据追加替换提示** | 否 | **否** | **是 —— 可用，外加"请改用 X"提示** | 我们主动引导调用者离开的退役名称，回放仍然安全 |
| **removed** | 完全未注册 | 否 | 否 | **否 —— 硬错误** | 仅在 `planned_removal_version` 之后、正式放弃回放支持时 |

### hidden-compatibility 与 deprecated —— 请精确区分

两种状态都**不可见**（不在 active 中，也不在工具搜索中），而且都保持**可分发**（调用仍然有效）。*唯一*的区别是面向调用者的信号：

- **hidden-compatibility：** 完全静默。工具的行为与其规范孪生逐字节相同。当*没有行为或命名上的教训需要传达*时使用它——该名称是纯别名，我们只是不想让模型重新学习它。（示例：`exec_wait` 就是字面意义上的 `exec_shell_wait`。）
- **deprecated：** 行为相同*且成功*，但工具结果的**元数据**会携带一条追加的提示，如 `"deprecated: use <replacement> instead"`。该提示**只**进入该次调用返回的结果元数据——绝不会进入缓存的工具目录前缀（见第 8 节）。当存在我们想让调用者（以及任何阅读转录的人）被引导过去的规范替代品时使用它。

两种状态都不会改变调用的*行为*。回放始终有效。

---

## 3. 代码中的表示

生命周期表示为 `crates/tui/src/core/engine/tool_catalog.rs` 中的 **const 名称集合加别名/清单表**，与现有的 `DEFAULT_ACTIVE_NATIVE_TOOLS`（`tool_catalog.rs:37-64`）和 `ARCEE_FIRST_TURN_NATIVE_TOOLS`（`tool_catalog.rs:106-115`）并列。

### 3a. 名称集合与清单（草图）

```rust
// crates/tui/src/core/engine/tool_catalog.rs  （计划中）

/// 从 active 集合和工具搜索中移除，但仍注册且可分发、行为逐字节相同的工具。静默。
pub(super) const HIDDEN_COMPATIBILITY_TOOLS: &[&str] = &[
    "exec_wait",          // == exec_shell_wait  (ShellWaitTool)
    "exec_interact",      // == exec_shell_interact (ShellInteractTool)
    "tts",                // == speech (SpeechTool)
    "work_update",        // == todo_write (TodoWriteTool)
    "TodoWrite",          // == todo_write (TodoWriteTool)
    "todo",               // == todo_write (TodoWriteTool)
    "checklist_write",    // == todo_write (TodoWriteTool)
    "checklist_update",   // == todo_write (TodoWriteTool)
];

/// 已废弃别名：不可见 + 可分发，替换提示只追加到结果元数据（绝不进入缓存前缀）。
pub(super) struct DeprecatedAlias {
    pub name: &'static str,
    pub replacement: &'static str,
    pub note: &'static str,
}

pub(super) const DEPRECATED_ALIASES: &[DeprecatedAlias] = &[
    // 在 #4132 工作面切换中为空：上面的旧名称是为转录回放而设的
    // todo_write 的静默隐藏兼容别名。
];

#[inline]
pub(super) fn is_hidden_or_deprecated(name: &str) -> bool {
    HIDDEN_COMPATIBILITY_TOOLS.contains(&name)
        || DEPRECATED_ALIASES.iter().any(|d| d.name == name)
}
```

### 3b. 两个过滤点

1. **目录 / 工具搜索排除（tool_catalog.rs）。** 延迟由 `should_default_defer_tool`（`tool_catalog.rs:66-82`）决定，active 集合是由 `build_model_tool_catalog`（`tool_catalog.rs:178-196`）构建的头部。hidden-compat 和 deprecated 工具必须被强制*移出 active 头部*和*移出工具搜索可发现的池子*。具体来说，延迟谓词获得一个短路分支，使这些名称永远不会 active，工具搜索索引构建器会跳过任何 `is_hidden_or_deprecated(name)` 为 true 的名称。Arcee 收窄的首回合路径（`apply_provider_tool_policy`，`tool_catalog.rs:134-149`）在构造上已经排除了它们，因为它们不在 `ARCEE_FIRST_TURN_NATIVE_TOOLS` 中。

2. **结果提示追加（tool_routing.rs）。** 分发已经按工具名路由（`crates/tui/src/tui/tool_routing.rs`，例如 `tool_routing.rs:1139-1140` 处的 wait/interact 统一）。成功分发后，如果被调用的名称在 `DEPRECATED_ALIASES` 中，路由器把匹配的 `note` **仅**追加到结果元数据。hidden-compat 名称不追加任何内容。

### 3c. 为什么用名称集合而不是每个 `ToolSpec` 的枚举字段

每个 `ToolSpec` 的 `lifecycle: Lifecycle` 字段因三个原因被否决：

- **前缀缓存安全。** 工具目录数组是 DeepSeek 不可变 KV 前缀的一部分（`tool_catalog.rs:169-177`）。逐规格字段会诱使把生命周期状态序列化进*每个*工具的 schema 中，这正是那种会强制完全重新填充的头部变更。名称集合完全存在于目录构建逻辑中，从不触碰发射出的工具 JSON。
- **单一事实来源 + 可差分性。** 一次发布的瘦身就是对单个文件中两三个 const 数组做一次小而可审查的编辑，而不是在众多工具模块中散落翻转字段。
- **注册保持正交。** 工具仍然与今天完全一样地注册（例如 `with_shell_tools`，`registry.rs:523-531`）。生命周期是叠加在注册之上的*目录策略*，不是烘焙进工具里的属性。

---

## 4. 废弃清单（#2681 验收标准表）

这是当初提议的清单。各列就是 #2681 的 AC 列。0.8.53 中没有条目被"removed"；清单中列出的每一项都计划支持回放。

> **部分被取代。** `exec_wait` 和 `exec_interact` 已被移除（`crates/tui/src/tools/registry/tests.rs` 中的 `shell_surface_exposes_lowercase_bash_and_hides_legacy_handler`）；它们不可调用。工作进度行并未全部移除。`todo_write` 是规范的模型可见 `TodoWriteTool`；`ToolRegistryBuilder::with_todo_tool` 还注册了隐藏回放别名 `work_update`、`TodoWrite`、`todo`、`checklist_write`、`checklist_update`。`checklist_add`、`checklist_list`、`todo_add`、`todo_update`、`todo_list` 未注册（引擎测试断言它们"必须不可调用"）。`registry/tests.rs`（`rlm_is_the_only_registered_session_surface`）中的 `"must no longer be callable"` 断言适用于旧的 `rlm_*` 会话名称，而非 checklist/todo。`tts` 通过 `with_speech_tools` 仍然可分发。`replay_supported = Yes` 列对 `tts` 以及已注册的 todo/checklist 别名为 true；对 `exec_wait`/`exec_interact` 和未注册的 checklist/todo 名称为 false。

| 别名 | 替代品（规范） | 生命周期状态 | first_deprecated_version | planned_removal_version | replay_supported |
|---|---|---|---|---|---|
| `exec_wait` | `exec_shell_wait` | hidden-compatibility | 0.8.53 | TBD（≥ 0.9.x） | Yes |
| `exec_interact` | `exec_shell_interact` | hidden-compatibility | 0.8.53 | TBD（≥ 0.9.x） | Yes |
| `tts` | `speech` | hidden-compatibility | 0.8.53 | TBD（≥ 0.9.x） | Yes |
| `checklist_write` | `todo_write` | hidden-compatibility | 0.9.0 | TBD（≥ 0.9.x） | Yes |
| `checklist_add` | `todo_write` | hidden-compatibility | 0.9.0 | TBD（≥ 0.9.x） | Yes |
| `checklist_update` | `todo_write` | hidden-compatibility | 0.9.0 | TBD（≥ 0.9.x） | Yes |
| `checklist_list` | `todo_write` | hidden-compatibility | 0.9.0 | TBD（≥ 0.9.x） | Yes |
| `todo_write` | `todo_write` | hidden-compatibility | 0.8.53 | TBD（≥ 0.9.x） | Yes |
| `todo_add` | `todo_write` | hidden-compatibility | 0.8.53 | TBD（≥ 0.9.x） | Yes |
| `todo_update` | `todo_write` | hidden-compatibility | 0.8.53 | TBD（≥ 0.9.x） | Yes |
| `todo_list` | `todo_write` | hidden-compatibility | 0.8.53 | TBD（≥ 0.9.x） | Yes |

`todo_*` 别名在 v0.8.53 首次进入隐藏兼容。v0.9.0 把它们的规范替代品改为 `todo_write`；但不会重置它们的 first-deprecated 版本。

**旧的子智能体名称 —— 已移除，无需清单条目。** 模型可见的子智能体面只有 `agent`。旧的（生命周期）名称和实验性工具智能体车道被移除，而不是保留为隐藏兼容工具。

`planned_removal_version` 有意保持为 `TBD`：一个名称只有在正式放弃对含它的旧转录的回放后才会进入 **removed** 状态，而这是对每个名称分别、审慎做出的决定。

---

## 5. 活动目录预算（按模式、按提供商）

active 集合就是首回合成本。不要在这里复述 `DEFAULT_ACTIVE_NATIVE_TOOLS` 的确切数量：v0.8.53 批次中的邻近 PR 可能增删 active 工具，而事实来源始终是 `tool_catalog.rs`。本文档定义的是瘦身策略和不变量，不是第二份目录快照。

### 按提供商

| 提供商 | 首回合 active 来源 | 预算策略 |
|---|---|---|
| Default（DeepSeek 等） | `DEFAULT_ACTIVE_NATIVE_TOOLS` | 当规范孪生保持 active 时，从 active 头部移除重复别名；任何净增长都需要显式的预算决定。 |
| Arcee（Trinity） | `ARCEE_FIRST_TURN_NATIVE_TOOLS` | 特定提供商的只读 WAF 变通方案；除非显式审查，否则不随默认瘦身改变。 |

默认瘦身把 `exec_wait` 和 `exec_interact` 从 active 头部移除（它们变成 hidden-compat；其规范孪生 `exec_shell_wait` / `exec_shell_interact` 保留）。`tts` 和旧的 `todo_*` 别名保持在 active 集合之外。规范的 `todo_write` 工具在 v0.9.6 变为 eager，作为显式的预算决定，这样普通的进度跟踪永远不需要一次发现回合。

### 按模式（Plan / Agent / YOLO）

原生 active 头部在设计上**跨模式是同一集合**——模式不会向 `DEFAULT_ACTIVE_NATIVE_TOOLS` 增删原生工具（`should_default_defer_tool` 对原生工具忽略 `_mode`，`tool_catalog.rs:66-68`）。模式改而影响 **MCP** 的延迟：`apply_mcp_tool_deferral` 让 MCP 工具保持延迟，除非 `mode == Yolo`（`tool_catalog.rs:162-167`）。

| 模式 | 原生 active 预算 | MCP 工具 active？ |
|---|---|---|
| Plan | 相同的原生头部 | 否（延迟） |
| Agent | 相同的原生头部 | 否（延迟） |
| YOLO | 相同的原生头部 | 是（已知且有意为之的拓宽） |

**预算规则：** 原生 active 头部在 Plan ↔ Agent ↔ YOLO 之间必须保持逐字节相同（第 8 节）。头部的任何增长都需要退役其他东西，或在本文档中显式上调预算。

---

## 6. 规范面规则

> **每个模型可见（active 或延迟可发现）的工具都必须有一个清晰的生态位。如果某个工具已被取代，它获得一个具名替代品并移入 hidden-compatibility 或 deprecated —— 它不会保持可见。**

### 容易混淆的簇的规范与兼容性小结

| 簇 | 规范（保持可见） | 兼容 / 已退役 | 备注 |
|---|---|---|---|
| **Shell wait** | `exec_shell_wait` | `exec_wait` → hidden-compat | 同一个 `ShellWaitTool`（`registry.rs:526,529`）；路由器已统一（`tool_routing.rs:1139`） |
| **Shell interact** | `exec_shell_interact` | `exec_interact` → hidden-compat | 同一个 `ShellInteractTool`（`registry.rs:527,530`） |
| **工作进度 / 清单 / todo** | `todo_write` | 已注册 hidden-compat：`work_update`、`TodoWrite`、`todo`、`checklist_write`、`checklist_update`；未注册：`checklist_add`/`list`、`todo_add`/`update`/`list` | 同一个 `TodoWriteTool`；已注册别名仅用于回放旧转录 |
| **Speech / tts** | `speech` | `tts` → hidden-compat | 同一个 `SpeechTool`（`registry.rs:787-792`） |
| **子智能体生命周期** | `agent` | 旧的（生命周期）名称和工具智能体车道已移除 | 单一的异步启动器。（此处"子智能体是叶子 worker"的说明并未落地 —— 见 §7。） |
| **编辑家族** | `apply_patch`、`edit_file`、`write_file`、`fim_edit` | 无 —— **生态位各不相同** | 未触碰（按 #2681 非目标）；仅文档层面的规范指引 |
| **搜索家族** | `grep_files`（内容）、`file_search`（文件名）、`project_map`（结构） | 无 —— **生态位各不相同** | 未触碰；目前不存在 FTS5/BM25/语义索引 |

**非目标（本周期明确不是瘦身目标，按 #2681）：** `apply_patch` / `edit_file` / `write_file` / `fim_edit`；`grep_files` / `file_search` / `project_map`；`fetch_url` / `web.run` / `web_search`；`task_shell_*`；`handle_read` / `retrieve_tool_result`。它们生态位各不相同，只接受**规范指引**——没有生命周期变更。

RLM 面（`rlm_open` / `rlm_eval` / `rlm_configure` / `rlm_close` / `rlm_session_objects`，`crates/tui/src/tools/rlm.rs`）同样不在范围内；`handle_read` 检索 var 句柄，`finalize` / `FINAL` 是内核内 Python 函数，**不是工具**——因此那里没有可退役的东西。

---

## 7. 子智能体切换决定：单一可见启动器

旧的（生命周期）三件套和工具智能体车道被移除，而不是保留为隐藏兼容工具。

**决定：只暴露 `agent`。**

- `agent` 启动一个专注的后台子智能体并返回 agent id 加转录句柄。
- 子智能体的结果以完成事件到达。父智能体应该继续工作，而不是轮询生命周期工具。
- 子智能体的工具目录排除被移除的子智能体*生命周期*工具。（**并非如落地所示：** 该要点原本继续写着"因此子智能体是叶子 worker，不能递归召唤更多智能体"。那不是最终结果。子智能体会收到 `agent`，并且可以递归到配置的深度——见 `tools/subagent/mod.rs` 中的 `with_full_agent_surface_options` 和 `can_spawn_child`，以及 [`SUBAGENTS.md`](./SUBAGENTS.md)。）
- 详细检查通过 `handle_read` 对返回的转录句柄进行。

这是生命周期简化，不是提供商层面的限制。

---

## 8. 前缀缓存安全 + 回放保证

### 每个瘦身 PR 必须遵守的前缀缓存规则

工具数组是 DeepSeek 不可变 KV 前缀的一部分。目录头部字节稳定性不变量（`tool_catalog.rs:169-196`）具有约束力：

1. **绝不非确定性地变更 active 头部。** 首回合 active 块在每次运行之间以及 Plan ↔ Agent ↔ YOLO 之间必须**逐字节相同**。
2. **瘦身是一次性的确定性编辑。** 从 `DEFAULT_ACTIVE_NATIVE_TOOLS` 移除一个名称恰好移动一次头部；之后它必须保持稳定。把这类编辑作为独立的聚焦变更落地。
3. **提示只存在于结果元数据中，绝不进入前缀。** deprecated 替换提示在分发时由 `tool_routing.rs` 追加到*调用结果*上。**任何**关于 hidden/deprecated 状态的内容都不得序列化进工具 schema、描述或目录数组。
4. **保持顺序和分区。** `build_model_tool_catalog` 按名称对每个分区排序，并把内建工具作为连续前缀保持在 MCP 工具之前（`tool_catalog.rs:186-194`）。瘦身编辑不得破坏这一点。
5. **hidden/deprecated 工具在头部构建*之前*被排除**，因此它们的移除是唯一的头部变更——它们完全不会出现在前缀中。

### 旧转录回放保证（未采纳）

下面的一揽子保证是提议，并未按原文落地。`exec_wait` 和 `exec_interact` 已被移除，调用会作为未知工具失败。工作进度回放更窄：`todo_write` 加上 `with_todo_tool` 注册的隐藏别名（`work_update`、`TodoWrite`、`todo`、`checklist_write`、`checklist_update`）仍然可分发；`checklist_add`、`checklist_list`、`todo_add`、`todo_update`、`todo_list` 不可。`tts` 保持可分发。`apply_patch` 是单独的仅回放编辑别名；见 [`TOOL_SURFACE.md`](../TOOL_SURFACE.md)。

> 对于废弃清单中 `replay_supported = Yes` 的每个名称，工具保持**以相同行为注册且可分发**。回放调用 `exec_wait`、`exec_interact`、`tts` 或任何 `todo_*` 的旧转录会产生与以往相同的结果。deprecated 名称额外附加结果元数据提示；hidden-compat 名称保持静默。一个名称只有在 `planned_removal_version` 做出放弃回放支持的审慎、逐名称决定之后，才会被设为不可分发（**removed**）。

---

## 9. 必需的测试

任何瘦身 PR（以及 #2681 总纲工作）都必须新增/保留：

1. **重复 active 别名护栏。** 一个测试断言 `HIDDEN_COMPATIBILITY_TOOLS` 或 `DEPRECATED_ALIASES` 中的任何名称都不出现在 `DEFAULT_ACTIVE_NATIVE_TOOLS` 或 `ARCEE_FIRST_TURN_NATIVE_TOOLS` 中，并且没有两个 active 条目解析到同一个底层工具实现。

2. **工具搜索排除测试。** 断言 hidden-compat 和 deprecated 名称不在工具搜索可发现的池子中，同时仍存在于注册表（可分发）中。

3. **回放 / 分发测试。** 对清单中的每个名称，调用它仍然执行并返回与其规范孪生相同的结果。deprecated 名称额外断言替换提示存在于**结果元数据**中，且不在目录/前缀中。hidden-compat 名称断言**没有**附加提示。

4. **黄金 active 块字节测试。** 一个快照测试钉住首回合 active 工具块的字节序列化，断言它在 Plan / Agent / YOLO（原生头部）之间相同且运行间稳定——强制 `tool_catalog.rs:169-196` 不变量。黄金文件**只**在瘦身落地时作为一次经过审查、审慎的一次性编辑更新。

5. **子智能体护栏测试。** 断言只有 `agent` 注册为模型可见的子智能体工具，并且 `subagent/mod.rs` 中的 hidden/旧名称不被通告。

6. **叶子 worker 测试。** 断言子智能体的工具目录排除 `agent` 和已退役的旧生命周期名称。
