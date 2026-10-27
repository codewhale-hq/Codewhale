# 自动工作流

> 英文原文：[AUTOMATIC_WORKFLOWS.md](../AUTOMATIC_WORKFLOWS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

要协调多个智能体（agent），你**不需要**自己写 `.workflow.js` 文件。小而紧耦合的工作
由 Operate 直接处理。多步委派从一份紧凑的工作流（workflow）计划开始：具名步骤、
依赖关系、有界范围，以及完成检查。工作流运行的就是 Fleet 所配置和管理的那批子智能体
（sub-agent），并在相互依赖的步骤之间传递结果与证据。一个独立且边界清晰的任务
可以直接用一个后台智能体；后续工作应当用 `followup` 复用那个智能体。Act/Agent
仍然可以使用下面描述的可选软自动策略。

相关文档：

- [工作流编写](WORKFLOW_AUTHORING.md) — 签入的脚本与 IR
- [Fleet + Workflow 教程](FLEET_WORKFLOW_TUTORIAL.md) — 手动的 Fleet 路径
- [配置](CONFIGURATION.md) — `[workflow]` 开关
- [沙箱](SANDBOX.md) — 工作流 VM 做不到的事情

## Act/Agent 中的软自动

1. **你用自然语言提要求**——“审计每个 crate 里的 unsafe”，“先侦察再实现”，
   “并行比较这两个提供商（provider）”。
2. **由 Codewhale 在 Act/Agent 中判断**——范围大、相互独立或分阶段的工作可以触发
   工作流；单文件编辑、简单命令和纯问答不会触发。
3. **它会先告诉你**——例如“这看起来适合用工作流——三个侦察智能体，再一个验证者。”
4. **可选的前置确认**——如果有一两个事实会改变计划（只读还是写入、范围、子项数量），
   它会打开 **`request_user_input`** 模态框（结构化单选／多选，而不是冗长的自由问答）。
5. **启动**——结构化的 `plan` JSON（goal / phases / children），或一段简短的内联脚本。
   并行分支使用 `parallel()` 的部分成功语义。

在 Operate 里，同样的诉求只要需要多个委派步骤，就会改用一份紧凑的工作流计划。
这份计划把并行工作、依赖交接，以及完成所需的证据一起摆到明面上。小而紧耦合的工作
可以留在父级，按当前生效的工具（tool）与审批（approval）策略执行；一个独立且边界清晰的任务
可以直接用一个智能体。任务没变时，用 `followup` 继续已有的智能体。你随时可以输入
`/workflow` 显式请求编排。

## 只读自动启动与写入审批

`[workflow]` 配置（见 `config.example.toml`）：

| 开关 | 默认值 | 含义 |
|------|---------|---------|
| `automatic` | `true` | 保留的兼容字段，目前不是启动开关；模型在当前工具和审批策略内选择编排。 |
| `auto_start_read_only` | `true` | 只读计划可以不带写入审批卡片直接启动 |
| `require_approval_for_writes` | `true` | 写入／提权启动前必须经过的计划审批卡片 |
| `max_children` / `max_concurrent` / `max_depth` | `0` / `16` / `0` | 总任务数及计划结构深度为 0 时不设上限；正值为操作者上限。超出并发额度的工作排队。Runtime 委派默认无产品深度限制，仍是独立的权限设置。 |
| `default_token_budget` | `0` | 回执保留的 token 提示；`0` 表示无提示。它不限制开销、子项运行时间或准入。 |

提权（elevated）工作（写入、只读之外的 shell、网络、机密、worktree、高预算）
在启动之前会给出审批卡片，上面写着目标、子项摘要、能力标志和预算（#4126）——
前提是 `require_approval_for_writes` 处于开启状态。这个标志只管这张卡片。
会话（session）级自动批准（YOLO / Full Access（完全访问）/ `bypass`）仍会跳过它，
和其他普通的 `Required` 工具一样。运行中的 VM `task()` 步骤内部的写入属于
VM 运行时契约（沙箱、`writeAuthority`、父级工具策略）——这个标志不会为每次
子项写入重新询问。

worktree 隔离与写入归属是两回事。具备写入能力的 `task()`
（`type: "implementer"`，或 `writeAuthority: "workspace_write"` /
`"worktree_write"`）可以声明仓库相对路径的 `writeRoots`、`exactFiles` 或
`coordinationContracts`；什么都不声明时，派生边界会认领它的 `deliverables`，
否则就认领工作区根目录（`.`），与普通的 Agent 派生完全一致。协调账本会拒绝
第二个存活写入者与之重叠的认领，所以当脚本在同一个检出目录里扇出并行写入者时，
应当给每个写入者不相交的 `writeRoots` 或 `exactFiles`。只读角色不能声明写入权限。
`worktree: true` 只是选择隔离，不会悄悄授予改动权限。纯提示词（prompt）的普通任务
是只读的。`dependencies` 和 `acceptance` 承载有界的、子项专属的前置条件与
可观察的完成检查；它们不是父级转录（transcript）的副本。

当一个工作流从包含多个仓库的工作区运行时，需要 shell 或文件访问的子项必须把 `cwd`
设成它应当使用的、相对仓库的目录。宿主（host）在派发之前会校验该目录确实存在于
父工作区内。隔离写入请用 `worktree: true`；`cwd` 只是选中一个已有的检出，
本身不授予写入权限，也不提供隔离。

## 控制一次运行

`/workflow status [run_id]`、`/workflow cancel [run_id]` 和
`/workflow settings` 由 Codewhale 自己根据运行日志和实时运行状态作答——
它们绝不消耗模型回合（turn），所以查状态是免费的，取消操作甚至在模型正忙时也能落地。
不带 id 的 `/workflow cancel` 会停掉当前唯一在跑的工作流。

开始工作走的是“先复审”的路子。`/workflow <objective>` 和裸 `/workflow`
会让模型给出一份有界、不调用工具的提案；`/workflow run
<path/to/x.workflow.js>` 则为那份确切的签入源码准备一次复审。这两种形式都不执行
任何东西。复审完提案之后，运行 `/workflow confirm` 启动最近一次复审过的草稿。
上文 `[workflow]` 表里的设置，在每次启动决策时（自动启动、写入审批卡片、子项上限）
都从你的 `config.toml` 读取；`/workflow settings` 会打印当前生效的值以及每一项的
作用。重新加载 `config.toml` 会刷新该表，对设置和工作流工具同时生效。

`/workflows` 打开运行面板：本工作区的日志为这个会话保留的每一次运行——
正在跑的和已完成的——最新的排在最前。每一行显示状态标记、运行标签、已用时间、
子项数量和最新进度；`Enter` 打开详情面板（运行 id、阶段、带每子项状态的子项名册、
近期进度，以及错误／结果摘要）。`x` 通过与 `/workflow cancel` 相同的宿主路径
取消选中的运行，`r` 重新读取日志，`Esc` 关闭。这个面板从不启动任何东西——
编排权力仍然留在 `/workflow` 手里。

## 运行期间你能看到什么

- **工作流面板**——阶段、子项、状态、预算
- **紧凑历史卡片**——一行平静的记录，展开看细节
- **每个委派单元一个工件（artifact）**——不重复出现“委派卡片 + 工具卡片”
- **带类型的子项身份**——标签／角色；默认界面里不会出现“未知子项”

取消会停掉该次运行和它的子智能体。已完成的活动可以跨会话留存
（配置之后也能跨重启留存）。

## 沙箱（sandbox）保证

工作流 JS VM **没有**文件系统、shell、网络、环境变量、import、时钟或随机数。
允许的宿主调用：`task`、`parallel`、`pipeline`、`phase`、`log`、
`budget`、`args`。真正的工作发生在子智能体／Fleet 里，遵守常规的工具与审批策略。
见[沙箱](SANDBOX.md)。

## 汇总与兼容性

- 必须返回结构化字段的只读子项，请优先用 `responseSchema`。写入子项仍须交回变更与验证证据。
- 普通的并行槽位失败会变成 `null`（部分成功）；在汇总成一份面向操作者的总结之前，
  先把它们过滤掉。`responseSchema` 不匹配属于契约失败，会故意让整次运行失败，
  而不是被悄悄转成 `null`。
- `null` 槽位不再匿名。`parallel()` 和 `pipeline()` 会给结果附加一个不可枚举的
  `errors` 数组——`[{ index, kind, message }]`，按 index 排序——这样汇总者就能说出
  某个槽位*为什么*缺失。数组自身的内容和 JSON 编码保持不变。
- `kind` 取值为 `admission`、`budget`、`cancelled`、`agent`、`schema`、
  `driver`（由故障所在的宿主赋值）或 `script`（脚本自己抛出的）。请从抛出的
  `Error` 的 `.kind` 读取；它从不依据消息文本推断，所以子项自己的措辞无法伪造 kind。
- `opts.mode` 选择契约：`settled`（默认——今天的行为）、
  `fail-fast`（以第一个非致命槽位错误拒绝整个扇出），
  或 `partial`（把每个非取消失败都解析为
  `{ __taskError: { index, kind, message } }`）。无法识别的模式会抛错，
  而不是悄悄按 `settled` 处理。
- 所有任务都失败的一次运行会被记为 **failed**，而不是部分成功，
  即使脚本本身返回了值。
- 原生计划中的单项、顺序和并行步骤会保留普通失败并继续独立工作。
  依赖失败或缺失结果的步骤会被跳过，不启动子项；回执保留可用结果与跳过原因。
  部分损失标为 `degraded`，没有可用工作标为 `failed`。取消和显式 schema 契约失败仍会停止运行。
- 旧 Workflow token 预算只保留解析与回执兼容，不再出现在工具或计划 schema 中，也不是强制上限。
  总量未计量：`budget.metered`
  为 `false`，`budget.spent()` 与 `budget.remaining()` 返回 `null`，`budget.total`
  保留声明的提示。提供商实际报告的用量仍保留在子项和运行回执中。
- 模型提供的步骤、时间、输出和委派深度上限不构成运行时权限。旧字段兼容解析但会被忽略，
  直接启动回执会说明忽略的字段，生成的 Workflow 任务不再携带旧 token、步骤或时间提示。
  真实限制来自操作者、角色/Fleet、父级或已保存运行的配置。
  停止操作仍然有效；继续工作直到目标完成，或遇到需要交回的实际阻碍。
- 兼容路径仍然保留：`script`、`source_path`（签入的
  `.workflow.js` / `.workflow.ts`），以及结构化的 `plan`。

## 选择编排

下列情况可以选择直接工具而非工作流：

- 单文件编辑和极小的单步请求  
- 简单命令／事实性问题  
- 高度交互的设计讨论  
- 没有清晰分解方式的风险型写入  
- 会超出 `max_children` / `max_depth` 的计划（在启动前就被拒绝）

这些是规划选择，不是任务大小的硬限制。按实际工作需要分解；只有配置的结构和权限检查会拒绝启动。

## 示例场景（#4131）

签入的示例工作流覆盖四个自动工作流场景：

1. 只读仓库审计  
2. 分阶段的缺陷修复，配 worktree 实现者 + 验证者  
3. 部分失败与汇总  
4. 运行中途取消  

夹具：[`docs/examples/dogfood-automatic/`](../examples/dogfood-automatic/)。
面板回归测试在 `crates/tui/src/tui/widgets/workflow_panel.rs` 中使用
`dogfood_` 前缀。
