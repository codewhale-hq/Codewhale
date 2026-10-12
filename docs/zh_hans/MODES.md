# 模式与权限姿态

> 英文原文：[MODES.md](../MODES.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

Codewhale 有三个相关概念：

- **TUI 模式**：你当前处于哪种可见交互（Plan/Work/Operate）。
- **权限姿态（permission posture）**：UI 在执行工具前主动询问的激进程度。
- **工作流（Workflow）**：通过命名步骤、依赖和结果协调子智能体，可在任何 TUI 模式中使用。

模型选择是独立的。`--model auto` 和 `/model auto` 把每个回合路由到具体的模型与思考强度；它们不是 TUI 模式，也不属于 `Tab` 循环。

Workflow 通过同一个子智能体运行时执行命名步骤；Fleet 管理这些子智能体的角色和模型配置。Workflow 额外提供执行顺序、结果交接、验证关卡和进度视图。当前模式和权限设置仍然决定每一步可以执行什么。

分配步骤前，先通过 `agent(action="roster")` 查看已保存的 Fleet 模型和角色。计划中的子智能体可以用 `model` 选择列表中的模型，也可以使用已保存的 `role`/`profile` 配置。Exact Fleet 会固定每个成员的路由。

## TUI 模式

按 `Tab` 补全输入框（composer）菜单，或在输入框为空时循环切换可见模式：**Plan → Work → Operate → Plan**。`Tab` 从不发送或排队输入框中的文本；用 `Enter` 发送或排队。按 `Shift+Tab` 循环切换权限姿态（Ask → Auto-Review → Full Access）。按 `Ctrl+T` 循环切换思考强度。运行 `/mode` 打开模式选择器，或直接用 `/mode work`、`/mode plan`、`/mode operate` 切换。

- **Plan**：设计优先的提示方式。原语（primitive）名称保持不变，仍是大家熟悉的那些，但运行时会集中拒绝文件修改和 shell 执行。只读检查与策略允许的研究（包括延迟加载的 Web 搜索/抓取）仍然可用。当一个 Plan 回合成功结束并给出非空回复（包括纯文字计划）时，TUI 会询问如何继续：Work（Ask）、Work（Auto-Review），或继续规划。选择 Work 会把已批准的原文作为普通后续消息发送，并保留该回合已有的 To-dos，或追加一个待办的计划项而不替换无关的工作。
- **Work**（内部为 `agent`）：普通的多步执行。第一回合的工具箱包含 `read`、`write`、`edit`、`bash`、`agent`、`workflow` 和 `todo_write`，以及无需先搜索即可使用的目标控制工具 `create_goal`、`get_goal` 和 `update_goal`。当请求属于一个持久的目标时，智能体会调用 `create_goal`；宿主从不根据措辞去推断目标，契约也不得告诉模型相反的内容。审批、沙箱、仓库规则（repository law）和托管策略决定什么可以执行。
- **Operate**：以全力运行的 Work，即“ultra”模式。工具与权限和 Work 相同，区别在于如何使用它们。每个实质性请求都会成为一个目标（`create_goal`），计划保持可见（`todo_write`），多部分工作通过 `workflow` 和并行的 `agent` 工作者执行，非平凡的改动在宣布完成前先交给独立的审查者，长时间运行的命令继续在后台运行，循环或监视类工作则作为 `automation` 提出（获批后创建）。它会一直进行，直到目标被验证、被你阻塞或被暂停。

`Act` 和 `/mode act` 仍然是 Work 的兼容别名。保存的设置仍然规范化为内部值 `agent`。

### 按模式划分的工具可用性

| 工具族 | Plan | Work | Operate |
|:---|:---:|:---:|:---:|
| `read` 与策略允许的延迟研究工具 | 是 | 是 | 是 |
| `write` 和 `edit` | 可见名称；执行被拒绝 | 受审批与策略门控 | 与 Work 相同 |
| `bash` | 可见名称；执行被拒绝 | 受审批与策略门控 | 与 Work 相同；并行或隔离有帮助时优先委托 |
| `agent` | 可以，受子智能体深度权限约束 | 可以，受子智能体深度权限约束 | 可以，受子智能体深度权限约束 |
| 延迟的原生、MCP 与插件工具 | 策略允许时可通过 `tool_search` 发现 | 相同 | 相同 |
| 付费或外部服务工具 | 遵循权限姿态 | 遵循权限姿态 | 遵循权限姿态 |
| 工作区根目录之外的访问 | 仅显式可信路径 | 仅通过可信路径或信任模式 | 与 Work 相同的可信路径/信任策略；Fleet profile 从不扩大它 |

Operate 改变的是调度重点，而不是权限。它既不增加特定于模式的工具拒绝，也不绕过当前生效的审批、沙箱、shell、询问规则、仓库规则或托管策略边界。Plan 仍然是 shell 与写入类工具唯一由模式划定的执行边界；这种权限差异并不需要另一套原语词汇。

### Operate 循环（一屏）

```text
用户消息
  → 小任务 / 对话 / 单文件？ → 父会话直接处理（与 Work 等价的工具）
  → 多步骤工作？ → 目标 → 命名步骤 + 依赖 + 完成检查
       → Workflow 阶段 → 独立子智能体并行执行
       → 收集结果 → 检查证据 → 交给下一阶段
       → 缺少必要结果？ → 停止依赖工作并修复该步骤
       → 单个独立任务？ → 直接委派一个子智能体
  → 父会话整合结果，汇报已完成、受阻和下一步
```

生命周期声明保持精确：已派发 ≠ 已定案 ≠ 已验证。

`allow_shell` 控制 `bash` 是否可以执行；它不重命名工具，也不让模式成为审批权威。持久任务与自动化在字段省略时保持保守的默认值，只有其设置显式授予时才获得 shell 权限。有状态的终端/后台控制是专门的延迟加载工具，而不是精简的前台 `bash` schema 上的字段。Full Access 改变的是权限姿态：确定性的 Auto-Review 底线只在会审查的姿态中生效，在 Full Access 下会被跳过，而仓库策略拦截仍然具有最终效力。

具备行动能力的模式可以通过 `tool_search` 发现延迟的 `rlm` 工具族；它的 `open`、`eval`、`configure` 和 `close` 动作拥有持久的 RLM 会话。旧的拆分式 `rlm_*` 名称仍是仅用于回放的别名。在 RLM Python REPL 内部，`sub_query_batch` 会扇出 1-16 个固定使用 `deepseek-v4-flash` 的低成本并行子调用。

快速的 `deepseek-v4-flash` / 关闭思考路径，在产品语言中叫 Fin。Fin 是用于路由、摘要、低成本子调用和协调工作的切入点；它不改变审批行为。

编排类控制不会占据起始界面，但仍然可用：`/auto` 打开 Auto-Review，让智能体直接开工；`/goal` 跨回合保持同一个目标；`/workflow` 准备一个可重复的有序或扇出工作流。它们都可以直接调用，也可以在完整命令面板中搜索到，但不会固定在起始斜杠菜单、空闲欢迎页、页脚或默认 Hotbar 上。单独输入 `/` 会打开一个精简的、面向任务的起始命令集；完整清单请用 `/help` 或命令面板查看。

`/goal <objective>` 设置一个带可选 token 预算的会话目标，并让处于活动状态的目标作为 Work 上下文保持可见。当直接请求描述一个需要多于一回合才能完成的可验证最终状态时（"直到测试通过"、"让 X 端到端工作"），智能体也可以自己创建目标；此时它会显示一行回执，你可以用 `/goal pause` 暂停或 `/goal clear` 清除它。裸 `/goal` 显示进度（状态、已用时间、延续次数，以及没有回合运行时如何继续）；没有目标且还没有对话时，打印用法。`/goal pause` 停止目标延续而不改变目标，`/goal resume` 恢复并把目标送回回合中，`/goal complete` 标记完成，`/goal blocked` 标记受阻，`/goal clear` 移除它。目标状态不改变活动的 TUI 模式、权限姿态或模型路由。这与只控制模型和思考强度选择的 `--model auto` 仍然是两回事。

工作流建立在同样的分离之上：目标可以让智能体持续工作，而 Workflow 为大规模扇出提供可重复的工作流和进度界面。在 UI 中，Workflow 运行应该作为主屏幕上的覆盖层显示，而不是作为 Plan、Work 和 Operate 旁边的另一个模式。

应用服务器（App-server）客户端可以用 `thread/goal/set` 持久化线程范围的目标，用 `thread/goal/get` 读取，用 `thread/goal/clear` 清除。该持久化记录携带 `active`、`paused`、`blocked`、`usage_limited`、`budget_limited` 或 `complete` 状态，外加供需要线程恢复语义的客户端使用的 token/时间计量字段。

## 模式持久化

交互式选择模式也会设置新会话启动时的模式。Tab/Shift+Tab 循环、`Alt+A` / `Alt+P` / `Alt+Y` 快捷键、Hotbar 的 Plan/Work/Operate 动作和 `/mode` 都会把 `default_mode` 写入 `~/.codewhale/settings.toml`，所以切换到 Operate 会在重启后保留。写入发生在事件循环之外；如果失败，TUI 会用警告提示（toast）说明，而不是在下次启动时静默回退。

模式、思考强度和模型选择器共享一个串行化的写入器，所以最后的选择就是磁盘上的选择——连续快速按 Tab 也不会让恰好最后完成的那次写入成为最终持久化的值——模式写入也永远不会回滚 `default_model` 等无关的键。

有两条路径故意**不**重写启动默认值：恢复已保存的会话（它会重新安装该会话所在的模式），以及因回合正在运行而被拒绝的模式更改。遗留的 `yolo` 入口点安装 Work 加 Full Access，它持久化的是 `agent`——`yolo` 是权限别名，绝不是启动模式。

重新选择你当前所处的模式并不是空操作。恢复会话后，活动模式和 `default_mode` 经常不一致，所以再次选择活动模式就是让它持久化的方式；Codewhale 会给出"已保存为启动默认值"的回执，而不是报告"已经在该模式"。

回合运行时，对当前路由的任何更改都会被拒绝——模式、模型、思考强度和提供商——无论你从哪个入口操作。这也包括斜杠命令入口（`/mode`、`/model`、`/config <key> <value>`、`/config preset`），它们在回合进行中同样可以触达。请先按 Esc 中断。仅重启的 `default_mode` 键豁免，因为它不触及正在运行的回合。

Codewhale 在跨进程的锁下写入 `settings.toml`，并以原子方式替换文件，所以同一主目录上的第二个 Codewhale 实例不会丢失你的选择，也不会读到写了一半的文件。退出时，排队写入在终端恢复前被刷新；任何失败的写入都会在退出时打印出来，而不是随备用屏幕一起消失。

## 兼容性说明

- 带有 `default_mode = "normal"` 的旧设置文件仍会按 `agent` 加载；保存时会改写为规范化后的值。

## Esc 键行为

`Esc` 是一个取消栈，不是模式开关。

- 先关闭斜杠菜单或瞬态 UI。
- 如果回合正在运行，取消活动的请求。
- 如果输入框为空，丢弃排队的草稿。
- 如果存在文本，清除当前输入。
- 否则不执行任何操作。

## 权限姿态

权限姿态控制工具审批，以及回合是否可以为缺失的用户决定而暂停。它是完整[授权顺序](./AUTHORIZATION_ORDER.md)中的一层，不是绕过工具准入、仓库规则或沙箱强制执行的手段。用 `Shift+Tab` 循环它，或在运行时编辑它：

```text
/config
# 把 approval_mode 行编辑为: suggest | auto | never
```

遗留说明：`/set approval_mode ...` 已被 `/config` 取代。

- `suggest`（**Ask**，默认）：工具审批可能打断你；当一个尚未解决的用户选择会实质性改变权限、成本、范围或结果时，Codewhale 会询问你。
- `auto`（**Auto-Review**）：自动审查工具调用。在交互式会话中，模型仍可通过 `request_user_input` 有意向用户提问；问题会挂起回合，直到用户回答、取消，或配置的超时到期。无头 `exec` 没有应答方，因此不提供这个工具。工具安全拦截与向用户提问是分开的。审批由两层决定。**确定性底线**（配置的阻断规则加内置安全底线）放行已证明安全的调用，并对发布类操作和破坏性的后台/无头工作直接硬阻断；这一层从不由模型审查。回退拦截——确定性引擎无法证明安全的调用——会升级给一次性的**模型守护者**（v0.9.8），由它返回风险级别、允许/拒绝和理由。守护者在各自独立的 JSON 字段中看到被拦截的确切调用和确定性观察；对话历史、技能指令、附件和展开的模型上下文都被排除在外。它不推断用户意图，也不计算通用的用户意图分数。高风险或严重风险即使模型判定允许，也不能自动运行。它没有工具，不记规则，遇到过大的确切调用会拒绝而不是截断。恰好只发出一次审查请求；输出不完整或格式错误、超时、取消或提供商失败，都会失败关闭（fail closed）。无头适配器只使用确定性层。明确要求由人来决定的仓库规则拦截，在 Auto-Review 中直接阻断，而不是弹出一个隐藏的审批弹窗。

LLM 审查器最接近 OpenAI Codex 的实验性 Auto-Review，对应提交 [`6fc6b9d6d2580d62622fc9884b5f5707f6505a5e`](https://github.com/openai/codex/tree/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e)。Codex 的 [guardian 入口点](https://github.com/openai/codex/blob/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e/codex-rs/core/src/guardian/mod.rs)重建对话上下文并运行专门的审查会话。Codewhale 有意只采用其中三点：针对确切动作的结构化决策、90 秒截止时间和失败关闭的结果。它不复制 Codex 的对话记录重建、用户授权分数、审查器工具、重试、持久审查会话或拒绝台账。

Kimi Code 在提交 [`1414d4602898f406e540b23342cb18db23ff9efc`](https://github.com/MoonshotAI/kimi-code/tree/1414d4602898f406e540b23342cb18db23ff9efc) 也没有 LLM 审查器。它的有序[权限策略](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/permissionPolicyService.ts)先应用显式拒绝规则，然后它的 [Auto 策略](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/policies/auto-mode-approve.ts)直接返回 `approve`。Codewhale 使用上述确定性安全底线与模型守护者，同时保留模型有意向用户提问的能力。

沙箱与升级基线参照 DeepSeek Harness `0.1.0-rc.5`，对应提交 [`47f943859bef60e4160492346772ded9b24f765a`](https://github.com/deepseek-ai/deepseek-harness/tree/47f943859bef60e4160492346772ded9b24f765a)：它的[sandbox 契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/sandbox.md)为每次调用定义 `read-only`、`workspace-write` 和 `danger-full-access` 边界，并禁止静默回退到无约束模式；它的[approval 契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/approval.md)只授予 `allowed-once`，并在被拒绝、被取消或没有可用的应答者时失败关闭；它的[sandbox 结果契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/shell/bash-sandbox/README.md)告诉模型：被拒绝的命令只重试一次，改用刚好够用的更宽模式，并附上理由。DeepSeek Harness 没有在这条路径上加入 LLM 审查器。Codewhale 的自主姿态只额外增加了上文所述的单次无状态守护者请求；确定性硬阻断仍然无法绕过。
- `bypass`（**Full Access**）：普通工具调用不显示审批提示，而用户有意发起的提问仍然可用。确定性的 Auto-Review 底线会被跳过——发布类命令、破坏性的后台/无头工作，以及 Windows npm 启动器平台拦截，都不会再拦下用户已经授予的调用。不可绕过的已注册拦截会自动批准，而不是弹出自相矛盾的弹窗。仓库规则拦截和托管策略拦截则作为硬阻断失败关闭，而不是用审批弹窗与 Full Access 自相矛盾。
- `never`：阻止任何不被视为安全/只读的工具；用户有意发起的提问仍然可用。

当前生效的姿态及其提问纪律，由门控工具的同一个运行时权威投射到每个回合，因此模式/姿态的更改在下一回合即可见。不可信的运行时生成输入会在构建元数据之前被收窄，无法凭空捏造审批权限。显式的 Full Access 子智能体交接会保留父会话的既定姿态，所以普通的子智能体工作不会重新开始弹出提示。

### 子智能体（sub-agents 与 Fleet worker）

子智能体会如实继承会话的姿态，而不是只继承一个简单的自动批准标志位：

- **Auto-Review**：worker 被拦截的调用走同一套确定性策略（已证明安全的调用运行；发布类和破坏性的后台工作被硬阻断）；对于无法证明安全的拦截，则使用该子智能体自己的会话客户端，交给同一个一次性的模型守护者。绝不会为子智能体弹出提示；守护者不可用时拒绝，即失败关闭。
- **Ask**：该角色可以委托的调用直接运行。被拦截的调用，在宿主是交互式 TUI 时，会作为审批提示在父会话的界面中提出（`agent:<id>:approval:<n>`）；worker 会明显地处于等待状态（`waiting for user`），用户的回答会被路由回它——无论父回合当时是空闲还是自己也在等待审批。无法弹出提示的宿主会拒绝，并说明原因。
- **Full Access**：调用会运行，包括破坏性的分离式工作——Auto-Review 底线与父回合一样被跳过。执行包络仍然生效，且永远不会放宽。

角色姿态和执行边界在这道关卡之前和之后都会被检查，且绝不会被放宽。凡是人没有在提示处亲自做出的决定，都会写入审计日志和子智能体的对话记录，作为一行说明（`Auto-Review allowed 'bash' (low risk, model guardian): …`），在聚焦该 worker 时可见。

## 小屏幕状态行为

终端高度受限时，状态区会先被压缩，让标题栏/对话区/输入框/页脚保持可见：

- 加载和排队的状态行按可用高度分配显示预算。
- 完整预览放不下时，排队预览折叠成紧凑摘要。
- `/queue` 工作流仍然可用；紧凑状态只影响渲染密度。

## 工作区边界与信任模式

默认情况下，文件工具被限制在 `--workspace` 目录。启用信任模式即可访问工作区之外的文件：

```text
/trust on
```

不带参数的 `/trust`（同 `/trust status`）只*报告*当前设置——不会启用任何东西。用 `/trust off` 再次限制访问。

Full Access 自动启用信任模式。

## MCP 行为

MCP 工具以 `mcp_<server>_<tool>` 暴露，使用与内置工具相同的审批流程。策略允许时，只读 MCP 辅助工具可以在 Ask 和 Auto-Review 中自动运行；可能有副作用的 MCP 工具需要审批。Full Access 不绕过硬性策略拦截。

工具自带的 MCP 注解，只在其来源可信时才被采信。来自已审查且已启用插件的工具，如果声明了 `readOnlyHint: true`，就像内置的只读辅助工具一样运行，不弹提示；来自其他任何服务器的同样声明会被忽略。声明了 `destructiveHint: true` 的工具永远得不到这种放宽，即使来自已审查的插件也一样，其审批卡片会写明该服务器把它标记为破坏性。Full Access 仍会不经提示直接运行它，与其他本来会询问的工具一样。

参见 [MCP.md](MCP.md)。

## 相关 CLI 标志

运行 `codewhale --help` 获取完整的规范列表。常见标志：

- `-p, --prompt <TEXT>`：一次性提示模式（打印后退出）
- `codewhale exec --auto --output-format stream-json <PROMPT>`：运行带工具的非交互式智能体，并为 harness 和后端封装层每行输出一个 JSON 对象。退出码：`0` 表示成功，`1` 表示真正的任务/智能体失败，`75`（`EX_TEMPFAIL`）表示回合因可重试的基础设施故障而结束（所有会话内重试都用尽之后，提供商/传输层出现 `network`/`timeout`），这样 harness 就能把可重试的基础设施类退出与任务失败区分开；流式输出末尾 `metadata` 事件中的 `error_category` 携带同样的分类
- `codewhale exec --prompt-file <PATH>` / `cat prompt.txt | codewhale exec --prompt-file -`：从文件或 stdin 读取提示词而不是命令行参数，用于超过操作系统单个参数长度上限（Linux 约 128 KiB）的提示词。不能与位置参数形式的提示词同时使用，位置参数 `-` 按字面文本处理；`--parent-death-watch` 占用 stdin，因此 `--prompt-file -` 与它一起使用会被拒绝
- `codewhale exec --resume <ID|PREFIX> <PROMPT>` / `--session-id <ID|PREFIX>`：非交互式继续一个已保存的会话
- `codewhale exec --continue <PROMPT>`：非交互式继续此工作区最近的已保存会话
- `codewhale fork <ID|PREFIX>` / `codewhale fork --last`：把已保存的会话复制为一个新的同级会话；分叉出的会话会额外保留父会话元数据，并在会话列表中显示这条谱系
- `--model <MODEL>`：通过 `codewhale` 统一入口使用时，向 TUI 转发 DeepSeek 模型覆盖
- `--workspace <DIR>`：文件工具的工作区根目录
- `-r, --resume <ID|PREFIX|latest>`：恢复一个已保存的会话
- `-c, --continue`：恢复此工作区最近的会话
- `--max-subagents <N>`：取值限制在 `1..=128` 之间
- `--mouse-capture` / `--no-mouse-capture`：启用或关闭内部的鼠标滚动、对话记录选择、右键上下文操作和对话记录滚动条拖动。在非 Windows 终端以及 Windows Terminal/ConEmu/Cmder 上，鼠标捕获默认启用，因此拖动选择只会复制对话记录中的文本，去掉段落中因视觉换行产生的断行，并且只作用于对话记录窗格；拖动时按住 Shift，或使用 `--no-mouse-capture`，即可进行终端原生的选择。在旧版 Windows 控制台（没有 `WT_SESSION` / `ConEmuPID` 的 CMD）和 JetBrains JediTerm（PyCharm/IDEA/CLion 等）中默认关闭，因为这些终端声称支持鼠标，却把 SGR 鼠标事件当作原始文本转发（#878、#898）。在默认关闭的环境中，可用 `--mouse-capture` 主动启用。终端原生选择可能越过右侧任务面板并包含视觉换行，因为此时选择由终端而不是 TUI 掌控。
- `--profile <NAME>`：选择配置 profile
- `--config <PATH>`：配置文件路径
- `-v, --verbose`：详细日志

## 分支与回滚

Codewhale 有三条相关、但有意彼此独立的恢复路径：

- `codewhale fork <ID>` 从现有已保存对话创建新的已保存会话，并记录源会话 id。这是在不覆盖原始会话的前提下探索另一条回答路径的安全方式。
- Esc-Esc 回溯会把实时对话记录倒回到之前的某条用户提示，并把该提示恢复到输入框中供编辑。它不会改动文件；之后运行 `/undo` 可以把该请求改过的文件改回去。
- `/restore` 和 `revert_turn` 工具从 side-git 快照恢复工作区文件。`/restore list [N]` 在选择回滚点前列出更多快照选项。它们不会改写对话历史。

Pi 风格的文件内树状浏览器是一个更大的 UI/数据模型项目。v0.8.40 交付的是范围有限的 fork/backtrack 原语和显式的谱系元数据。
