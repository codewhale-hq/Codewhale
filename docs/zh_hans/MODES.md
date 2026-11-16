# 模式与权限姿态

> 英文原文：[MODES.md](../MODES.md)。
> 最后与英文同步日期（last synced with English revision）：2026-11-15。

Codewhale 有三个相关概念：

- **TUI 模式**：你当前处于哪种可见交互（Plan/Work）。
- **权限姿态（permission posture）**：UI 在执行工具前主动询问的激进程度。
- **工作流（Workflow）**：通过命名步骤、依赖和结果协调子智能体，可在任何 TUI 模式中使用。

模型选择是独立的。`--model auto` 和 `/model auto` 把每个回合路由到具体的模型与思考强度；它们不是 TUI 模式，也不在权限拨盘上。

Workflow 通过同一个子智能体运行时执行命名步骤；Fleet 管理这些子智能体的角色和模型配置。Workflow 额外提供执行顺序、结果交接、验证关卡和进度视图。当前模式和权限设置仍然决定每一步可以执行什么。

分配步骤前，先通过 `agent(action="roster")` 查看已保存的 Fleet 模型和角色。计划中的子智能体可以用 `model` 选择列表中的模型，也可以使用已保存的 `role`/`profile` 配置。Exact Fleet 会固定每个成员的路由。

## TUI 模式

一个拨盘决定 Codewhale 可以不经询问做多少事。按 `Shift+Tab` 逐档切换：**Ask → Accept Edits → Plan → Auto-Review → Full Access → Ask**。Plan 档就是 Plan 模式；其余每一档都是带该权限姿态的 Work。页脚显示当前档位和 `Shift+Tab to change`。未保存权限姿态时，新安装从 Accept Edits 开始。显式保存的选择（包括旧版本中的 Ask）在升级和重启后保持不变。`Tab` 只补全输入框（composer）菜单、命令、文件提及和建议；它不会切换模式，也从不发送或排队输入框中的文本（用 `Enter`）。每次有效权限变化都会在对话记录中写入一行 `Permissions changed to …`。按 `Ctrl+T` 循环切换思考强度。运行 `/mode` 打开模式选择器，或直接用 `/mode work`、`/mode plan` 切换。`operate` 及其写法仍按 Work 解析，并说明 Operate 已退役，所以旧的设置或脚本不会报错。选择器分为两组：**对话模式**列出引擎提供的标准模式与创作者模式（暂不可用的模式会列出并置灰，同时给出原因），**权限**包含 Work 与 Plan（仍为 `1`、`2`）。对话的模式在第一条消息之前选定，之后固定：空白对话可用 `/mode creator` 或选择器就地更改；发送第一条消息后选择器锁定该组，`/mode creator` 会被拒绝并提示开启新对话。`/new` 开始标准对话，`/new creator` 开始创作者模式对话（仍可配合 `--force`）。恢复、`/load`、`/fork` 和 `/clear` 保留已保存的对话模式；非标准模式的名称会显示在页脚。模式不会改变任何权限。`codewhale` 与 `codewhale exec` 接受 `--conversation-mode <MODE>`；与 `--resume`、`--session-id` 或 `--continue` 同用时必须与已保存的模式一致，否则在任何模型调用之前以退出码 2 结束。

在创作者模式下，`plugin_manager` 的只读查询在各权限姿态下均可无提示执行，而变更类插件调用（`install_bundle`、`remove_bundle`、`set_bundle`、`set_plugin`）会先在 `ToolExecutionPlan` 上做无副作用的只读准备，再由引擎在执行前完成门控。工作区作用域内的本地变更走 Auto-Review 的守护者／allow 规则或在 Full Access 下执行（并在插件状态中记录智能体姿态）；作用域外变更——包括项目外路径、远端源或含未解析 npm 依赖的本地包（审批前绝不拉取，且安装后默认保持禁用和未受信任）、禁用 shell 执行时声明了 stdio MCP 连接器或钩子的插件包、由人批准过的插件以及内置开关——都会跳过守护者，即使处于 Auto-Review 或 Full Access 也直接向人弹出审批卡片；`Plan`、`sandbox_mode = "read-only"`、`Never`、`Background`、子智能体及嵌套的 `execute_tools` 则拒绝变更类插件调用。

- **Plan**：设计优先的提示方式。原语（primitive）名称保持不变，仍是大家熟悉的那些，但运行时会集中拒绝文件修改和 shell 执行。只读检查与策略允许的研究（包括延迟加载的 Web 搜索/抓取）仍然可用。当一个 Plan 回合成功结束并给出非空回复（包括纯文字计划）时，TUI 会询问如何继续：Work（Ask）、Work（Accept Edits）、Work（Auto-Review），或继续规划。选择 Work 会把已批准的原文作为普通后续消息发送，并保留该回合已有的 To-dos，或追加一个待办的计划项而不替换无关的工作。
- **Work**（内部为 `agent`）：普通的多步执行。第一回合的工具箱包含 `read`、`write`、`edit`、`file_search`、`grep_files`、`bash`、`agent`、`todo_write`、`present` 和 `load_skill`，以及无需先搜索即可使用的目标与后台 shell 控制工具 `create_goal`、`get_goal`、`update_goal` 和 `task_shell_wait`。当请求属于一个持久的目标时，智能体会调用 `create_goal`；宿主从不根据措辞去推断目标，契约也不得告诉模型相反的内容。审批、沙箱、仓库规则（repository law）和托管策略决定什么可以执行。
- **Operate 已退役。** 它曾是带编排约定的 Work，而这份约定现在存在于每个会话的宪章中：把持久请求变成目标，保持计划可见，把独立工作扇出给并行子智能体，并让独立审查者尝试反驳非平凡的改动。保存的 `default_mode = "operate"` 会按 Work 加载并提示一次；Runtime API 中 `mode: "operate"` 的请求会以 `agent` 加默认姿态（Accept Edits）运行，每个进程记录一次提示，绝不报错。从旧 Operate 对话分叉出的智能体不会继承其约定。多智能体工作在所有模式下都通过 `agent`、`workflow` 和 `/fleet`（舰队）进行。

`Act` 和 `/mode act` 仍然是 Work 的兼容别名。保存的设置仍然规范化为内部值 `agent`。

### 按模式划分的工具可用性

| 工具族 | Plan | Work |
|:---|:---:|:---:|
| `read` 与策略允许的延迟研究工具 | 是 | 是 |
| `write` 和 `edit` | 可见名称；执行被拒绝 | 受审批与策略门控 |
| `bash` | 可见名称；执行被拒绝 | 受审批与策略门控；并行或隔离有帮助时优先委托 |
| `agent` | 可以，受子智能体深度权限约束 | 可以，受子智能体深度权限约束 |
| 延迟的原生、MCP 与插件工具 | 策略允许时可通过 `tool_search` 发现 | 相同 |
| 付费或外部服务工具 | 遵循权限姿态 | 遵循权限姿态 |
| 工作区根目录之外的访问 | 仅显式可信路径 | 仅通过可信路径或信任模式；Fleet profile 从不扩大它 |

模式改变的是调度重点，而不是权限。没有哪个模式会增加特定的工具拒绝，或绕过当前生效的审批、沙箱、shell、询问规则、仓库规则或托管策略边界。Plan 仍然是 shell 与写入类工具唯一由模式划定的执行边界；这种权限差异并不需要另一套原语词汇。

### 编排循环（一屏）

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

工作流建立在同样的分离之上：目标可以让智能体持续工作，而 Workflow 为大规模扇出提供可重复的工作流和进度界面。在 UI 中，Workflow 运行应该作为主屏幕上的覆盖层显示，而不是作为 Plan 和 Work 旁边的另一个模式。

应用服务器（App-server）客户端可以用 `thread/goal/set` 持久化线程范围的目标，用 `thread/goal/get` 读取，用 `thread/goal/clear` 清除。该持久化记录携带 `active`、`paused`、`blocked`、`usage_limited`、`budget_limited` 或 `complete` 状态，外加供需要线程恢复语义的客户端使用的 token/时间计量字段。

## 模式持久化

交互式选择模式也会设置新会话启动时的模式。`Shift+Tab` 拨盘、`Alt+A` / `Alt+P` / `Alt+Y` 快捷键、Hotbar 的 Plan/Work 动作和 `/mode` 都会把 `default_mode` 写入 `~/.codewhale/settings.toml`，所以切换到 Plan 会在重启后保留。写入发生在事件循环之外；如果失败，TUI 会用警告提示（toast）说明，而不是在下次启动时静默回退。

模式、思考强度和模型选择器共享一个串行化的写入器，所以最后的选择就是磁盘上的选择——连续快速按 Shift+Tab 也不会让恰好最后完成的那次写入成为最终持久化的值——模式写入也永远不会回滚 `default_model` 等无关的键。

恢复已保存的会话故意**不**重写启动默认值：它会重新安装该会话所在的模式。遗留的 `yolo` 入口点安装 Work 加 Full Access，它持久化的是 `agent`——`yolo` 是权限别名，绝不是启动模式。

重新选择你当前所处的模式并不是空操作。恢复会话后，活动模式和 `default_mode` 经常不一致，所以再次选择活动模式就是让它持久化的方式；Codewhale 会给出"已保存为启动默认值"的回执，而不是报告"已经在该模式"。

回合运行时，模式和权限可以从任何入口更改（Shift+Tab、`/mode`、Hotbar、`/config mode …`），包括把拨盘切入或切出 Plan。更改不会重启正在流式输出的回复，引擎会在同一回合的下一步应用它：下一批工具调用之前、每个审批关卡，以及下一次模型请求之前（该请求带有新的模式提示）。状态行会说明更改要等到那一步才生效。

模型、思考强度和提供商在回合运行时仍会被拒绝，无论从哪个入口（`/model`、`/provider`、`/config <key> <value>`、`/config preset`）：它们会改变正在运行的请求所在的路由。请先按 Esc 中断。

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

权限姿态控制工具审批，以及回合是否可以为缺失的用户决定而暂停。它是完整[授权顺序](./AUTHORIZATION_ORDER.md)中的一层，不是绕过工具准入、仓库规则或沙箱强制执行的手段。用 `Shift+Tab` 拨盘切换它，或在运行时编辑它：

```text
/config
# 把 permission_posture 行编辑为: ask | accept-edits | auto-review | full-access
```

遗留说明：`/set approval_mode ...` 已被 `/config` 取代。

- `ask`（**Ask**；内部为 `suggest`）：工具审批可能打断你；当一个尚未解决的用户选择会实质性改变权限、成本、范围或结果时，Codewhale 会询问你。每次文件编辑和每条命令都会询问，包括工作区内的编辑。
- `accept-edits`（**Accept Edits**，默认；线上值 `accept_edits`）：与 Ask 相同，但所有目标都在工作区 git 工作树内的文件编辑无需卡片即可运行。对 `.git` 内部、`.codewhale`、`.env*`、凭据和密钥文件的编辑始终询问；在没有 git 的文件夹里，每次编辑也都询问（卡片会说明该文件夹没有仓库）。命令、网络、MCP 操作和文件夹外的任何操作都与 Ask 一样询问；ask 规则、hook、仓库规则和安全底线仍会强制弹出卡片。这对应 Claude Code 的 `acceptEdits`。ACP 会话按 Ask 运行它，因为 ACP 回合从不应用文件夹内编辑规则。
- `auto`（**Auto-Review**）：自动审查工具调用。在交互式会话中，模型仍可通过 `request_user_input` 有意向用户提问；问题会挂起回合，直到用户回答、取消，或配置的超时到期。无头 `exec` 没有应答方，因此不提供这个工具。工具安全拦截与向用户提问是分开的。每个回合都带有引擎铸造的运行来源（仅当由已附着所有者界面的 TUI 输入框或已登录所有者运行时界面直接发起或引导时为 `Interactive`；`exec` 和 ACP 回合包括 `approval_policy = "auto"` 在内均为 `Headless`；分离启动、分离式子智能体、定时任务、自动化及延迟的 `send_later` 回合均为 `Background`）和所有者在场标志（`owner_present`），宿主或请求字段永远不能指定这两项。审批由两层决定。**确定性底线**（配置的阻断规则加内置安全底线）放行已证明安全的调用（包括明确列入例行范围的工作区测试与构建命令，含末尾的 `2>&1` 或 `>/dev/null` 及单个 `head`、`tail` 或字面 `grep` 过滤——不包括 `cargo run`、非例行 `make` 目标、`git add`、`git commit`、丢弃改动标志或你自己的阻断规则），对包与发行版发布（`npm publish`、`cargo publish`、`gh release` 及发布工具）、非交互或无人值守的破坏性操作（`Background`、`Headless` 或 `owner_present` 为假的回合）以及任何无人值守的仓库推送与标签变更直接硬阻断，在有所有者在场（`owner_present`）的交互式回合中将仓库远端推送连同 Auto-Review 命名软规则（`force-push`、`git-tag`、`deploy-branch`、`git-discard`、`infra-destroy`、`ci-or-pr-bypass`、`print-credentials`、`secret-commit`、`deploy-wrapper`）优先于 `auto_review.allow` 拦下交由守护者审查，并拦截关键路径删除（文件系统根目录、主文件夹、项目根目录或其任意上层目录以及 `.git` 目录），在有所有者在场的交互式会话中会询问你，在无人值守或无头会话中则直接拒绝；并且仅在 Auto-Review 下，即使命中 `auto_review.allow` 规则，针对 `AGENTS.md`、`CLAUDE.md` 或已加载指令文件（含符号链接和子目录）的文件工具或 shell 写入、编辑、删除或移动也会被拦下交由守护者审查；针对受保护路径（`.git`、`.codewhale/`、凭据与环境密钥文件、`.vscode/`、`.claude/`、`.mcp.json`、`.claude.json`、`.github/workflows/`、`.gitconfig`、`.config/gh/hosts.yml`、shell 启动/配置脚本以及 `Library/LaunchAgents/`、`.config/autostart/`、`.cargo/config.toml`、`.config/fish/`、`.config/systemd/user/`、Windows 启动项，含符号链接与别名）的文件或 shell 写入/修改会在窄精确放行、允许规则与例行命令之前交由守护者审查（不改变 Ask、Accept Edits、Plan 与 Full Access）；在无 git 的文件夹中，仅当启用本地快照（`snapshots_enabled`）且工作区根目录安全（非 `/`、`$HOME`、其上层目录或桌面/文档等顶层用户目录）时允许常规有界写入，而删除或清空整个文件以及关闭快照或根目录不安全时的写入仍交由守护者审查（Accept Edits 仍要求工作区 git 工作树）；读取 `.claude.json` 或 `.config/gh/hosts.yml` 会触发 `print-credentials` 软规则；首次解析到工作区之外的 shell 读取（忽略 `/dev/null` 等标准伪设备）在存在可提示的所有者时会弹窗请求一次性确认（离开 Auto-Review、切换模式或重置/回退会话时撤销），无交互所有者或不可弹窗的运行则直接交由守护者审查；Ask、Accept Edits、Plan 和 Full Access 保持不变。内置安全底线从不由模型审查，也不会在其余拦截或无人值守操作上等待审批卡片。回退拦截——确定性引擎无法证明安全的调用、交互式仓库推送、出站网络请求、命名软规则拦截以及 Auto-Review 下受保护指令文件的修改——会升级给一次性的**模型守护者**，由它返回风险级别、允许/拒绝和理由（命名软规则会在拦截与阻断回执中写出规则名称，并要求你对该操作提出具体请求）。守护者在 64 KiB 预算内看到按不受信任证据标注的独立 JSON 字段：Rust 记录的所有者输入框原文（`person_messages`，最新在前）、已批准的计划文本（`approved_plan`）、仅来自用户或受管配置的 `environment` 范围条目（项目级 `.codewhale/config.toml` 无法设置或修改 `auto_review.environment`）、当前回合此前经脱敏的非只读动作（`recent_actions`，不含工具输出）、会话启动时固定的用户级与仓库级指令（分开标注并附 SHA-256 哈希，回合途中不重读磁盘）、确定性观察（含 `person_approved_retry` 与触发的 `soft_rule`）以及被拦截的确切调用（保留 `description`）。工具输出、助手文本、展开的 `@` 文件内容、技能正文、暂停注释、导入的对话记录以及非所有者或系统生成的提示（定时任务、自动化、`send_later`、目标延续、子智能体简报、审查任务、聊天中继与非所有者运行时客户端）均排除在 `person_messages` 之外。高风险或严重风险除非经你针对该精确调用授予一次性重试授权，否则不能自动运行；即使已授权重试，守护者显式拒绝仍会阻断。它没有工具，不记规则，若当前请求或确切调用超出预算则直接返回不可用并失败关闭而不截断；在预算压力下按仓库指令、用户指令、中间较旧用户消息的顺序裁剪，同时保留最早的限制（例如“不要 push”）与最新请求。守护者请求通过 `auto_review.reviewer_model` 或 `/config auto_review.reviewer_model` 指定（未设置时在同一提供商路由内选择有能力的模型，或回退到当前会话模型，从不跨提供商或跨账户切换）；输出不完整或格式错误、超时、取消或提供商失败，都会失败关闭（fail closed）。每次 Auto-Review 阻断都会点名规则与原因，并提示改走其他路径，或在所有者在场的交互式可重试阻断上通过回执快捷键 `r`（空输入框）、`/permissions blocked` 或 Runtime API（`POST /v1/threads/{id}/auto-review/approve`）授予该精确调用的一次性重试授权。每次重试授权仅绑定到精确的规范 `(tool_name, call_digest)`，重试时消费一次，仍会经过所有非 Auto-Review 关卡（钩子、带类型权限、仓库规则、确定性底线与沙箱），永不向子智能体或扩展调用继承，并在离开 Auto-Review 或重置对话时撤销。当 `owner_present` 为真（且非子智能体）时，连续 3 次阻断、会话累计 20 次阻断或连续 3 次审查不可用会将权限切换为 Ask（`AutoReviewPaused`）并在 Ask 下继续当前批次（切回 Auto-Review 会重置计数）。无人值守运行（`owner_present` 为假）绝不回退到 Ask；它会越过阻断继续尝试其他路径，并在累计 20 次阻断或连续 10 次审查不可用时以“已中断”结束并列出被阻断动作摘要。一次放行会重置连续阻断和连续不可用计数，而明确的守护者拒绝会重置连续不可用计数。无头 `exec` 回合在将无人值守破坏性操作交给确定性规则的同时，仍会对回退的非破坏性拦截咨询模型守护者；ACP 回合只使用确定性层并拒绝守护者审查。当 `owner_present` 为真（交互式 TUI 或所有者运行时界面回合）且未命中硬阻断（`hidden_unicode` 或配置的 `auto_review.block` 规则）时，非扩展自身的强制提示（带类型 `permissions.toml` 的 `ask` 规则、`tool_call_before` 钩子的 `ask`、Computer Use 同意与脚本卡片等仅限提示工具，以及明确要求人工参与的仓库规则拦截）会弹出正常的审批卡片，而不再被跳过或失败关闭（在运行时线程上，只有所有者界面 `desktop`、`tui` 或 `web` 可以回答这类卡片）。在 Auto-Review 下，需要审批的扩展调用（含嵌套 `core/call`、宿主服务操作及插件宿主工具）会带着宿主铸造的插件身份交由守护者结合上下文审查，且绝不会通过例行放行、`allow` 规则或一次性重试授权跳过（独立的带类型规则、钩子拦截与安全底线阻断依然生效）。当 `owner_present` 为假（`Background`、`Headless` 或未附着回合）时，无人值守与无头回合绝不弹出审批卡片，强制提示会点名该规则拒绝且不登记待决审批，明确要求由人来决定的仓库规则拦截也会直接阻断。

LLM 审查器最接近 OpenAI Codex 的实验性 Auto-Review，对应提交 [`6fc6b9d6d2580d62622fc9884b5f5707f6505a5e`](https://github.com/openai/codex/tree/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e)。Codex 的 [guardian 入口点](https://github.com/openai/codex/blob/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e/codex-rs/core/src/guardian/mod.rs)重建对话上下文并运行专门的审查会话。Codewhale 有意只采用其中四点：针对确切动作的结构化决策、90 秒截止时间、失败关闭的结果，以及按回合计数的拒绝熔断。它不复制 Codex 的完整助手/工具对话记录重建、用户授权分数、审查器工具、重试、持久审查会话或拒绝台账，而是只传入经 Rust 核验的所有者输入、已批准计划、固定指令和同回合此前动作摘要。

Kimi Code 在提交 [`1414d4602898f406e540b23342cb18db23ff9efc`](https://github.com/MoonshotAI/kimi-code/tree/1414d4602898f406e540b23342cb18db23ff9efc) 也没有 LLM 审查器。它的有序[权限策略](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/permissionPolicyService.ts)先应用显式拒绝规则，然后它的 [Auto 策略](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/policies/auto-mode-approve.ts)直接返回 `approve`。Codewhale 使用上述确定性安全底线与模型守护者，同时保留模型有意向用户提问的能力。

沙箱与升级基线参照 DeepSeek Harness `0.1.0-rc.5`，对应提交 [`47f943859bef60e4160492346772ded9b24f765a`](https://github.com/deepseek-ai/deepseek-harness/tree/47f943859bef60e4160492346772ded9b24f765a)：它的[sandbox 契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/sandbox.md)为每次调用定义 `read-only`、`workspace-write` 和 `danger-full-access` 边界，并禁止静默回退到无约束模式；它的[approval 契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/approval.md)只授予 `allowed-once`，并在被拒绝、被取消或没有可用的应答者时失败关闭；它的[sandbox 结果契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/shell/bash-sandbox/README.md)告诉模型：被拒绝的命令只重试一次，改用刚好够用的更宽模式，并附上理由。DeepSeek Harness 没有在这条路径上加入 LLM 审查器。Codewhale 的自主姿态只额外增加了上文所述的单次无状态守护者请求；确定性硬阻断仍然无法绕过。
- `bypass`（**Full Access**）：普通工具调用不显示审批提示，而用户有意发起的提问仍然可用。确定性的 Auto-Review 底线会被跳过——发布类命令、破坏性的后台/无头工作，以及 Windows npm 启动器平台拦截，都不会再拦下用户已经授予的调用。不可绕过的已注册拦截会自动批准，而不是弹出自相矛盾的弹窗。仓库规则拦截和托管策略拦截则作为硬阻断失败关闭，而不是用审批弹窗与 Full Access 自相矛盾。
- `never`：阻止任何不被视为安全/只读的工具；用户有意发起的提问仍然可用。

当前生效的姿态及其提问纪律，由门控工具的同一个运行时权威投射到每个回合，因此模式/姿态的更改在下一回合即可见。不可信的运行时生成输入会在构建元数据之前被收窄，无法凭空捏造审批权限。显式的 Full Access 子智能体交接会保留父会话的既定姿态，所以普通的子智能体工作不会重新开始弹出提示。

### 子智能体（sub-agents 与 Fleet worker）

子智能体会如实继承会话的姿态，而不是只继承一个简单的自动批准标志位：

- **Auto-Review**：worker 被拦截的调用走同一套确定性策略（已证明安全的调用运行；发布类和破坏性的后台工作被硬阻断）；对于无法证明安全的拦截，则使用该子智能体自己的会话客户端，交给同一个一次性的模型守护者。绝不会为子智能体弹出提示；守护者不可用时拒绝，即失败关闭。当运行过至少一次工具调用的子智能体返回到处于 Auto-Review 的父智能体时，守护者还会对照根会话所有者消息（`person_messages`）、已批准计划与固定项目指令，对其已执行动作列表做一次返回审查（仅含工具名与经裁剪/脱敏的调用入参，绝不包含工具结果或子智能体报告正文）。若审查发现风险或无法运行（不可用、超时、取消或超出守护者上下文预算），引擎会在返回给父智能体的报告顶部前置经过清洗的 `[Auto-Review warning: ...]` 警告横幅。子智能体执行期及返回审查产生的 Auto-Review 阻断都会以 `[sub-agent <id>]` 条目记录到父会话的已阻断动作列表（`/permissions blocked` 与 `GET /v1/threads/{id}/auto-review/blocks`）中，且不可授予一次性重试授权（`approvable: false`）。
- **Ask** 和 **Accept Edits**：该角色可以委托的调用直接运行。被拦截的调用，在宿主是交互式 TUI 时，会作为审批提示在父会话的界面中提出（`agent:<id>:approval:<n>`）；worker 会明显地处于等待状态（`waiting for user`），用户的回答会被路由回它——无论父回合当时是空闲还是自己也在等待审批。无法弹出提示的宿主会拒绝，并说明原因。
- **Full Access**：调用会运行，包括破坏性的分离式工作——Auto-Review 底线与父回合一样被跳过。执行包络仍然生效，且永远不会放宽。

角色姿态和执行边界在这道关卡之前和之后都会被检查，且绝不会被放宽。凡是人没有在提示处亲自做出的决定，都会写入审计日志和子智能体的对话记录，作为一行说明（`Auto-Review allowed 'bash' (low risk, model guardian) [reviewer: …]: …`），在聚焦该 worker 时可见。

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
- `codewhale exec --auto --output-format stream-json <PROMPT>`：运行带工具的非交互式智能体，并为 harness 和后端封装层每行输出一个 JSON 对象。`--auto` 默认启用 Full Access（完全访问；没有 Codewhale shell 沙箱，开启信任模式）；加上 `--sandbox workspace-write` 可保留 shell 沙箱和文件工具的工作区限制，并关闭信任模式。除 `danger-full-access` 外的任何 `--sandbox`（或配置中对应的 `sandbox_mode`）都会保持信任模式关闭；`external-sandbox` 依赖你自行提供的隔离。向模型提供工具的 exec 运行会在所有输出格式下保存。退出码：`0` 表示成功，`1` 表示失败。网络和超时错误不能授权监督进程重复执行命令：原请求可能已经执行，或用户消息可能已经保存。流式输出末尾的 `metadata` 事件保留 `error_category` 供诊断使用，但它不是幂等性回执。
- 请把 exec 标志放在提示词前。以 `-` 开头的提示词需要先加上 `--`，例如 `codewhale exec -- "- explain this option"`。单独的 `-` 是字面文本。提示词之后出现的已知 exec 长标志（包括 `--flag=value`）会被拒绝，除非它们位于显式的 `--` 之后，此时才按字面提示词文本处理。
- `codewhale exec --prompt-file <PATH>` / `cat prompt.txt | codewhale exec --prompt-file -`：从文件或 stdin 读取提示词而不是命令行参数，用于超过操作系统单个参数长度上限（Linux 约 128 KiB）的提示词。不能与位置参数形式的提示词同时使用，位置参数 `-` 按字面文本处理；`--parent-death-watch` 占用 stdin，因此 `--prompt-file -` 与它一起使用会被拒绝
- `codewhale exec --resume <ID|PREFIX> <PROMPT>` / `--session-id <ID|PREFIX>`：非交互式继续一个已保存的会话
- `codewhale exec --continue <PROMPT>`：非交互式继续此工作区最近的已保存会话
- `codewhale fork <ID|PREFIX>` / `codewhale fork --last`：把已保存的会话复制为一个新的同级会话；分叉出的会话会额外保留父会话元数据，并在会话列表中显示这条谱系
- `--model <MODEL>`：通过 `codewhale` 统一入口使用时，向 TUI 转发 DeepSeek 模型覆盖
- `--workspace <DIR>`：文件工具的工作区根目录
- `-r, --resume <ID|PREFIX|latest>`：恢复一个已保存的会话
- `-c, --continue`：恢复此工作区最近的会话
- `--max-subagents <N>`：取值限制在 `1..=128` 之间
- `--permission-mode <ask|accept-edits|plan|auto-review|full-access>`：以指定的权限模式启动（同时适用于 `codewhale` 和 `codewhale exec`）。`codewhale exec --auto` 仍表示 Full Access。当受管配置设置了 `auto_review.disabled = true` 时，`Shift+Tab` 会跳过 Auto-Review，且 `--permission-mode auto-review`、`/auto` 和 `approval_policy = "auto"` 都会被拒绝。
- `--mouse-capture` / `--no-mouse-capture`：启用或关闭内部的鼠标滚动、对话记录选择、右键上下文操作和对话记录滚动条拖动。在非 Windows 终端以及 Windows Terminal/ConEmu/Cmder 上，鼠标捕获默认启用，因此拖动选择只会复制对话记录中的文本，去掉段落中因视觉换行产生的断行，并且只作用于对话记录窗格；拖动时按住 Shift，或使用 `--no-mouse-capture`，即可进行终端原生的选择。在旧版 Windows 控制台（没有 `WT_SESSION` / `ConEmuPID` 的 CMD）和 JetBrains JediTerm（PyCharm/IDEA/CLion 等）中默认关闭，因为这些终端声称支持鼠标，却把 SGR 鼠标事件当作原始文本转发（#878、#898）。在默认关闭的环境中，可用 `--mouse-capture` 主动启用。终端原生选择可能越过右侧任务面板并包含视觉换行，因为此时选择由终端而不是 TUI 掌控。
- `--profile <NAME>`：选择配置 profile
- `--config <PATH>`：配置文件路径
- `-v, --verbose`：详细日志

## 分支与回滚

Codewhale 有三条相关、但有意彼此独立的恢复路径：

- `codewhale fork <ID>` 从现有已保存对话创建新的已保存会话，并记录源会话 id。这是在不覆盖原始会话的前提下探索另一条回答路径的安全方式。
- Esc-Esc 回溯会把实时对话记录倒回到之前的某条用户提示，并把该提示恢复到输入框中供编辑。它不会改动文件；`/restore` 可以把文件夹回滚到所选的恢复点，需要 `/trust on` 或 Full Access。
- `/restore` 和 `revert_turn` 工具从 side-git 快照恢复工作区文件。`/restore list [N]` 在选择回滚点前列出更多快照选项。它们不会改写对话历史。

Pi 风格的文件内树状浏览器是一个更大的 UI/数据模型项目。v0.8.40 交付的是范围有限的 fork/backtrack 原语和显式的谱系元数据。
