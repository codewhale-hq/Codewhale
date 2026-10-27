# Agent Fleet

> 英文原文：[FLEET.md](../FLEET.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

Agent Fleet 是面向持久化多 worker 运行的本地优先名册（roster）与成员选择层。它**不会**执行或授权任何工作。Fleet 确定谁应当参与之后，由受委派的协调者（coordinator）启动一次无头的 `codewhale exec` 运行，再由 Runtime 持久地跟踪它。关于子智能体、`exec` 与 fleet 支撑的 worker 如何汇聚到同一个运行时，请参阅 [AGENT_RUNTIME.md](./AGENT_RUNTIME.md)。在产品语言里，用户仍然可以“打开一个子智能体”；在架构语言里，持久的嵌套工作使用的是带委派运行时执行的 fleet 成员身份。

## 命名与兼容边界

**Fleet** 是面向公众的产品名词。持久账本、已保存的名册、配置表和 `--fleet` 标志都使用这个名字：

| 表面 | 规范写法 |
| --- | --- |
| CLI | `codewhale fleet …` |
| 斜杠命令 | `/fleet …` |

凡是改名会破坏现有工作区、回执或脚本的地方，这些共用名称都是承重的，不能改：

- 持久账本 `.codewhale/fleet.jsonl`，以及日志目录 `.codewhale/fleet/` 和 `.codewhale/fleet-host/`；
- 已保存的名册 `fleets/<name>.toml` 及其 `schema = "fleet"` 头部，位于 `$CODEWHALE_HOME/` 或工作区的 `.codewhale/` 下（工作区根目录 `fleets/` 中已提交的名册仍会被读取）；
- `[fleet]` 配置表（内联的 `[fleets.*]` 表已在 0.9.14 移除；具名 fleet 存放在 `fleets/<name>.toml` 文件中）；
- `codewhale workflow run --fleet <name>` 标志；
- 线路（wire）、回执和控制平面的操作 id，例如 `fleet.status`。

本文其余部分把 fleet 用作面向公众的产品名词，并保留这些字面上的路径、键、标志和 id。

当一次委派运行需要在重试、睡眠/重启、远程执行、回执或有账本（ledger）的审计轨迹之间保持稳定的成员身份时，请使用 fleet 名册，而不是匿名的短命 `agent` 扇出。初始 CLI 表面如下：

关于结合 fleet 任务规范与 Workflow 编写的、从启动到监控的引导式演练，请参阅 [fleet + Workflow Tutorial](./FLEET_WORKFLOW_TUTORIAL.md)。

```sh
codewhale fleet init
codewhale fleet run tasks.json --check   # validate only; nothing is created or launched
codewhale fleet run tasks.json --max-workers 4
codewhale fleet status
codewhale fleet inspect <worker-id>
codewhale fleet logs <worker-id>
codewhale fleet artifacts <worker-id>
codewhale fleet interrupt <worker-id>
codewhale fleet restart <worker-id>
codewhale fleet resume <run-id>
codewhale fleet stop --all
```

`codewhale fleet resume <run-id>` 是重启恢复命令：它回放账本，对任何进行中的、其 worker 已停止心跳的租约（lease）进行对账（在任务预算内重试，否则按告警策略失败并升级），然后打印恢复后的状态。它不会启动任何新工作，并且是幂等的，因此在 manager 退出、笔记本休眠或运行时重启之后运行它都是安全的。

fleet 支撑的运行的协调者状态存储在工作区下的 `.codewhale/fleet.jsonl`。worker 日志与 adapter 日志存储在 `.codewhale/fleet/` 与 `.codewhale/fleet-host/` 下。

## 公共契约：身份、成员与选择

**Fleet** = 用户的模型清单：名册里有谁，以及选中了哪个成员。

一个公开的 fleet 身份只包含：

- 一个稳定的成员 id 和一个可选的面向用户的名称；
- 一个语义角色（role），例如 `explore`、`implement` 或 `reviewer`（旧写法 `worker`、`scout`、`builder`、`verifier`、`consultant` 和 `oracle` 在输入时仍被接受，并分别映射到 `general`、`explore`、`implement`、`test` 和 `advisor`）；
- 一个确切的 provider/model 身份，或一条显式继承的 route；
- 可见的名册状态或来源。

项目或工作区信任、文件系统与网络的可达范围、机密访问、approval 模式、沙箱、工具授权，以及其他一切形式的运行时权限，都是独立的委派协调与 Runtime 策略输入。它们绝不是 fleet 身份字段，也绝不会选择或改道某个 fleet 成员。Runtime 只在成员选定之后才应用并收紧这些策略；如果选中的成员无法在生效的权限范围内运行，启动会以 fail closed 失败，而不是改选别人。

自然语言的成员选择是确定性的。调用方可以指名：

- 一个确切的成员 id，可写成 `member:<id>` 或 `id:<id>`；
- 一个唯一的、面向用户的成员名称，可写成 `name:<name>`；
- 一个唯一的语义角色，例如 `explore` 或 `role:explore`；
- 一个确切的固定 model id，例如 `deepseek-v4-flash`，或其离线显示名，例如 `DeepSeek V4 Flash`；或
- 一个确切的 `route:<provider>/<model>`。

不带限定的确切成员 id 优先。其他每种匹配只有在恰好指向一个不同的名册成员时才成功。多个匹配会产生歧义错误，列出候选并要求使用 `member:<id>`；Codewhale 绝不会挑选碰巧排在最前面的匹配。用户不需要知道 `explore` 这类内部角色标签：唯一的成员名称、显示 model 或确切的 model id 同样有效。已保存的 v2 fleet 把这个可选的人类可读名称存为 `display_name`（输入别名 `name` 也被接受）；它必须是一行经过修剪的可打印文本，最长 80 个字符。

### 你的 fleet 就是模型

同一个 fleet 文件还回答第三个问题：**这个人把哪些模型放进了自己的 fleet？** 选中 fleet 中每一个确切的 `provider` + `model` 固定项——operator route、每个固定的成员，以及每个显式标记的候选（shortlist）行——都是一个 fleet 模型。可执行的成员行提供它所承担的角色；候选行没有角色。所有这些都保留在同一个 fleet 文件里。省略 `role` 的旧成员仍以其 id 作为角色身份。候选行只携带模型选择；思考档位（reasoning）、指令和能力要求属于可执行的角色成员，在候选行上会被拒绝。

- `/fleet models` 打印 fleet：`provider/model · roles · price · context · tools`，这些事实读自模型目录。没有选中 fleet 时，该行显示“Your fleet is the session model only”。
- `/fleet add <provider> <model> [role…]` 添加一个模型（每个角色一行成员，无角色的添加则是一行 `shortlist = true`）。该 provider 必须是你已配置的；并且当目录已知该 provider 时，它必须确实提供这个确切的 id。当角色成员被要求运行 fleet 自己的 operator route 时，它会继承该 route 而不是固定它——operator 变动时角色随之变动；固定到任何其他 route 则是有意的退出。已经固定 operator route 的文件按继承来读取。没有选中 fleet 时，会先创建并选中一个名为 `My fleet` 的用户级 fleet。`/fleet remove <provider> <model>` 会删除所有固定该 route 的行；operator route 用 `/fleet save` 更改，不能被移除。
- 在 `/model` 中，`⇧F` 为该确切 route 添加或移除一个显式候选行。它会保留已保存的角色固定项和 operator route；fleet 模型排在最前，标注为 `fleet · <roles>`，位于你自己的 `⇧P` 固定项和各 provider 列表之前。`/models` 会在 provider 列表之前先打印 fleet。

operator 模型在分配子智能体时读取这份列表（设计文档 `MODEL-ROUTING-CATALOG-20260901.md` §10，切片 F2）。面向模型的名册通过与启动时相同的 route 准入代码来解析角色。显式配置与手动的角色固定项仍然具有权威性；一个唯一的已保存角色固定项也适用于只指名该角色的启动。对于未固定的角色，可以选择任务级 model，但限于选中的模型加上会话 route。候选模型行各自独立地披露其确切 route，与任何角色固定项无关。

### 交互式与持久状态

`/fleet status` 与 `codewhale fleet status` 是同一条命令在两个表面（surface）上的体现。两者都通过一份共享的控制平面契约读取工作区持久的 `.codewhale/fleet.jsonl` 账本，并报告相同的动词 id（`fleet.status`）、读/写权限、持久化范围与回执。当工作区没有账本时，它们会用带类型的理由（`no_fleet_ledger`）说明这一点，而不是渲染出看似“一切正常”的空状态——而且两者都不会因为读取而顺带创建账本。

当前交互会话的子智能体是**另一组**对象，现在它们有自己的名字：

- `/fleet workers`（或 `/subagents`，或在 `/fleet` 名册中按 Tab / `w`）显示附着在当前 TUI 会话上的子智能体。它不读取持久账本。
- `/fleet list|status|interrupt|resume` 与 `codewhale fleet list|status|interrupt|resume` 作用于持久账本。
- `codewhale fleet restart <worker-id>` 仅限 CLI：它重新租用该任务，然后驱动 manager 循环直至完成。`/fleet restart` 不会默默做一个更小的动作——它会报告 `surface_not_supported` 并指出对应的 CLI 命令。

这背后的契约——描述符、可用性理由、精确身份目标、回执、带类型的未知项与边界——记录在 [`docs/COMMAND_CONTROL_PLANE.md`](../COMMAND_CONTROL_PLANE.md)。

## 编写 agent 配置（`/fleet setup`）

给智能体的说明：持久的产物是下文描述的配置 TOML；逐项键的演练是面向人的交互路径。

`/fleet setup`（也可以是 `/fleet setup edit` / `new`）打开一个 TUI 内向导，用于编写可复用的智能体团队配置。裸 `/fleet` 以及 `roster`/`roles`/`profiles`/`party` 别名会打开选中 fleet 的成员名册。`/fleet saved` 打开具名的已保存 fleet 选择器。`/fleet workers` 打开当前会话的 worker 视图；`/subagents` 是该视图的兼容快捷方式。要查看持久的运行历史，请使用上文描述的 `/fleet status` 或 shell 命令 `codewhale fleet status`——它们是同一条命令。

该向导是渐进式的：你每次只做一个聚焦的选择——先是 **role**，然后是 **model**（`inherit`，或来自*任何已配置 provider* 的具体模型，不限于父会话当前正在使用的那一个），然后是**配置存放在哪里**，最后是对成员身份与 route 的 **review**。当 review 还预览 thinking、工具、approval 或其他执行控制时，这些行概括的是独立的 Runtime 策略；它们不会成为 fleet 身份或成员选择器。每一步的头部都显示“Saves to: …”——要么是你还需要做出的选择，要么是你做出选择后解析出的确切文件。在 review 步骤激活保存控件之前，什么都不会写入。

**Destination** 步骤是一个聚焦的两选项列表：

- **This project** 写入 `<workspace>/.codewhale/agents/<role>.toml`。它只作用于当前项目，并优先于 id 相同的 Personal 配置。当会话禁用了项目配置（`--no-project-config`）或工作区文件夹不可用时，该选项会连同理由一起显示为禁用；向导绝不会自行回退到 Personal。
- **Personal** 写入 `$CODEWHALE_HOME/agents/<role>.toml`，在本机所有项目中可用，除非某个项目有 id 相同的自有配置。

对于高亮选项，该步骤会显示确切文件、保存将新建文件还是**替换现有文件**，以及对名册的优先级影响。review 步骤会在“Saves to”下重复这些事实，并按效果命名最终动作——**Save to this project**、**Save as Personal profile** 或 **Replace …**。替换现有文件需要在保存控件上再确认一次。从 `/fleet` 重新打开已保存的成员时，会从磁盘上的内容开始：它的成员身份、route 和保存范围。Thinking（`inherit`、`off`、`low`、`medium`、`high`、`max` 或 `auto`）在 review 步骤调整，但它仍是 route 的执行设置，而不是成员 fleet 身份的一部分。

配置范围（profile scope）控制角色定义在何处可复用；它不会扩大正在运行的操作的权限，也不是项目信任设置。要协调几个相邻的仓库，请从它们的共享父目录启动 Codewhale，使该父目录成为工作区。项目/工作区信任、外部路径、文件系统与网络可达范围、机密、approval、沙箱和工具授权，都来自委派协调与 Runtime 策略。对于嵌套委派，Runtime 会把请求的子级权限档位（posture）与实时的父级取交集。对于独立的 `codewhale fleet` 执行，Runtime 则使用由任务显式写入范围生成的有界工具权限范围，并结合实时配置、沙箱和平台强制措施。两条路径都不会从配置的存储范围或身份选择器读取权限。范围授予只读 shell 访问的 worker，运行的是与会话内只读智能体相同的只读命令语法，包括管道、链式命令和开头的 `cd`（见 `docs/SUBAGENTS.md` 中的“Read-only shell commands”）；被准入的 `gh` 读取还需要范围的网络授权。`npm view` 仍在只读语法之外，因为 npm 配置可以选择可执行的辅助程序；网络授权并不授权这些辅助程序。

选择具体模型会显式固定其 provider：保存的配置同时记录 `model` 与 `provider` 字段，因此它命名的 route 不依赖于配置稍后加载时碰巧处于活动状态的 provider。在 review 步骤按 **Enter**（“start”）会在同一屏内联预览确切的首个配置 TOML；在你保存之前什么都不会写入。`provider` 字段可以是内置 provider id（如 `openrouter`），也可以是配置在 `[providers.<name>]` 下的用户命名 OpenAI 兼容 provider（如 `lm-studio`）；启动路径保留该 id，并在 provider 未配置时 fail closed。

配置也是面向模型的 `agent` 工具选择 route 的方式：子任务要么以 `profile` 运行（精确使用其保存的 route 与 thinking 档位），要么继承 operator 的 model。对于未固定的角色，每个任务的 `model`、`model_strength` 和 `thinking` 仍会对外公布；已保存的配置和手动的角色固定项会拒绝覆盖。对外公布的字段列表与解析时接受的兼容列表见 docs/SUBAGENTS.md。

当配置了 provider 时，review 步骤还会在显式的保存前预览门控之后提供模型辅助起草：

- 按 **`m`** 让第一个已配置 model 起草配置。草稿到达时已被净化且有界。另外，无论模型提议什么，Runtime 都保持其保守的执行底线（无 shell 或信任升级，且需要 approval）。
- **起草不等于保存。** 精确渲染的 TOML 预览会内联显示在 review 步骤（而不是单独的滚动查看器），因此只有按 **`g`** 或 **Enter** 保存（或再按 `m` 重新起草）才会真正保存。保存会把配置写入预览中显示的项目或个人范围。

## 命名：Modes、Workflow 与 Fleet

这些名字描述的是不同的层次，而非互相竞争的系统。Plan 与 Work 是日常工作的模式。Operate 接受普通消息，并在与 Work 相同的 approval、沙箱、shell、ask 规则与仓库保护之下保留父级的正常工具表面。它倾向于为独立、并行、隔离或长时间运行的工作使用后台 fleet worker，但并不要求每个可执行步骤都配一个 worker。Workflow 是一个可选的编排叠加层，用于需要排序、门控、共享预算、回放或确定性汇入（fan-in）的工作。

简短的公开口径如下：

- **Fleet** 是持久的名册与确定性的成员选择表面。它记录成员 id 与名称、语义角色、provider/model 身份以及名册状态。Fleet 也是存储与线路格式所使用的名字。
- **Workflow** = 工作按什么顺序执行：phase、gate、预算、回放与 fan-in。
- **Lane** = 一个正在运行的 Workflow 实例及其实时进度。
- **Runtime** = 被选中的工作在何处、如何、以何种权限执行。Runtime 负责本地或远程进程、provider route、项目/工作区信任、文件系统、网络、机密、approval、沙箱、工具和 API 边界。

- **Workflow** 是可重复的计划与面向用户的编排叠加层：一个决定接下来运行哪些 phase 和智能体的脚本/IR，把中间结果挡在主对话之外，并且可以检查或重跑。一次 Workflow 运行应该有可见的进度视图和清晰的活动头部状态，而不是像一个隐藏的后台任务。
- **Fleet** 是持久的名册与确定性的成员选择表面：成员 id 与名称、语义角色、固定或继承的 provider/model 身份，以及名册状态。受委派的协调者与 Runtime 负责启动并发、租约、心跳、日志、回执、工具、沙箱、approval 与权限。
- **High fan-out** 是 Workflow 运行的一种行为，而不是独立系统：当一个 phase 需要同时很多 worker 时，Workflow 会把它们作为 fleet 支撑的运行（持久 worker、回执、目标再派发）派发，而不是复活仅靠提示词的子智能体扇出。
- **Fan-in 是显式的：** 当用户需要一个合并结果时，由 owner 聚合、验证并综合各 worker 回执。独立任务可以各自完成；派发绝不等于完成。

UI 指引：保持主 transcript 平静。一次 Workflow 运行应显示为紧凑的进度卡片加上任务面板行（输入区下方的条带，或侧边的任务面板），包含 phase 名、worker 数、回执，以及为子 worker 准备的嵌套缩进。鲸鱼标记（whale mark）应克制地用作活动头部/状态信号；避免为每个 worker 重复堆砌 emoji 行。

## 已保存的 fleet 与 Reasoning Router

选中的 v2 fleet 会在 Workflow 启动前，把每个选中成员的 id、语义角色、provider 和 model 身份冻结进持久运行。把 fleet 保存为工作区 `.codewhale/` 下的 `fleets/<name>.toml`（fleet 编辑器保存文件夹 fleet 的位置），或 `$CODEWHALE_HOME` 下；工作区根目录中已提交的 `fleets/<name>.toml` 也会被读取。模型无法在运行时替换这些身份或 route 指派：

```toml
schema = "fleet"
schema_revision = 2
name = "release"

[operator]
provider = "deepseek"
model = "deepseek-v4-pro"

[[members]]
id = "implementer"
display_name = "Release Builder"
role = "implement"
provider = "zai"
model = "glm-5.2"

[[members]]
id = "advice"
role = "advisor"
provider = "openai"
model = "gpt-5.6"
```

workflow crate 较旧的 `schema = "exact"`、revision 1 文件仅作为迁移输入。不要再编写 revision 1 文件；选中的名册和设置 UI 只读写 `schema = "fleet"`、revision 2。

`workflow(fleet: "release")` 会运行一个已保存的 Fleet 而不选中它。在 Workflow 启动时，没有固定项的成员采用该 Fleet 的 `[operator]` route，若没有，则采用会话的 route 与思考档位；这条被冻结的 route 就是实际运行的、也是回执所记录的 route，运行中途编辑文件只会影响下一次 Workflow。如果一个已保存的 Fleet 与较旧的 exact/legacy 文件同名，Workflow 会拒绝猜测；请把已保存的那个限定为 `user/<name>` 或 `folder/<name>`。带有 `instructions` 或 `requires` 的成员目前还不能在 Workflow 中运行。

思考档位（reasoning）是独立的 route 执行决策，不属于 fleet 身份。可选的 Reasoning Router 是可复用的 Runtime 服务，不是 fleet 成员。把一份配置保存在任一搜索根下的 `routers/<name>.toml`，并让任意数量的 fleet 引用它：

```toml
name = "luna-low"
schema = "reasoning_router"
schema_revision = 1
provider = "openai"
model = "gpt-5.6-luna"
call_reasoning = "low"
```

在运行时，它只能为一条已经冻结的 worker route 选择思考档位。它不能改变成员、provider、model 或语义角色。Router 调用本身被限制在 `off` 或 `low`；更贵的值会被拒绝。手动选择的 worker 思考档位不会产生 Router 调用。Route 与思考回执会指出 worker 的 model，并在使用 Router 时给出 Router 的确切 provider/model，让操作者看到哪个 model 干了哪份活。如果相同的裸 Router 或 fleet 名字在多个根里都存在，请把它限定为 `codewhale_home/<name>`、`workspace/<name>`（工作区的 `.codewhale/`）或 `workspace_root/<name>`（工作区根目录），而不是依赖遮蔽（shadowing）。

兼容性 schema 可能会在成员旁序列化 `reasoning`、`permissions`、工具提示或其他执行设置。这些值不是 fleet 身份、成员选择器或生效的权限。一个有效的旧版 `schema = "exact"` 名册快照，只在验证和回放该快照记录的内容哈希时保留其旧的 `permissions` 字节；新的捕获会输出不含权限的成员形态。新运行的校验会拒绝旧版名册的 `security_policy` 与 worker 的 `trust_level` 字段；请通过 Runtime 策略配置执行权限。受委派的协调者先解析并持久地冻结成员，随后 Runtime 再应用委派父级生效的上限，或者对于独立的 fleet CLI 工作，应用 Runtime 执行配置加上实时的沙箱/平台强制措施。该边界可以缩小或拒绝选中 worker 的执行表面，但绝不能选择不同的成员或 route。强制执行契约见 [`docs/MODES.md`](../MODES.md)、[`docs/SUBAGENTS.md`](../SUBAGENTS.md) 与 [`docs/AGENT_RUNTIME.md`](../AGENT_RUNTIME.md)。

思考回执记录被请求的档位*以及*实际向 provider 请求的档位。只要 route 无法表达被请求的档位，这两者就会不同——Codewhale 的 route 归一化器在大多数 route 上会把请求的 `low` 发送为 `high`，而 Z.AI 的 GLM route 只能表达 thinking 开/关——因此回执报告真实请求，而不是被选中的标签。调用实际携带的值由该 route 自己的归一化器拼写，而不是由档位标签决定：OpenAI Codex route 被请求 `xhigh` 而不是 `max`，并且根本无法被请求 `off`。

持久的 fleet CLI 回执把选中的配置 id 保存在 `effective_permissions.profile_id`，把解析出的语义角色保存在 `resolved_route.role`，并把生效的 Runtime 表面记录在 permission、shell 和 tool-scope 字段中。精确的 Workflow 启动回执把 `member_role` 与可选的 Runtime `posture_role` 分开记录，并附上在 spawn 边界检查的生效权限范围的指纹。因此，名为 `auditor` 的成员可以保留这个身份，而 Runtime 报告 `custom` 权限档位，并独立证明它所强制执行的更窄表面。

Workflow 启动时对任何可以在本地判定的事情 fail closed：无法解析的 provider 或 model、缺失的凭据、无法为成员 route 构建的客户端，或没有可用 Reasoning Router 的 `auto` 成员。按任务的验证——那些 spawn 边界本来就会拒绝的东西，特别是没有声明 `write_roots`/`exact_files`/`coordination_contracts` 的可写成员——会在调用 Router 之前检查，因此无效任务永远不会消耗一次路由请求。如果 spawn 在 Router 决策*之后*失败，回执仍会被记录：token 已经花了，任何跨 provider 披露也已经发生。

## Manager 拥有的 Workflow fan-in

当并行工作必须返回一个合并答案时，优先使用 manager 拥有的 Workflow，而不是扁平的 `agent` 扇出。默认形态：

1. **指定一个 manager**（operator 或 workflow 编排器）。
2. 通过 `workflow`（`task()`、`parallel()`、`pipeline()`、`phase()`）或一个拥有这些子任务的单一 manager 会话**扇出**子任务。
3. **等待**子任务回执或完成事件。
4. 在把承载结论的主张当作事实之前，**聚合并验证**它们。
5. **综合**出一个操作者可以依赖的结果。

裸 `agent` 扇出适合没有合并结果的独立工作。当结果必须合并、比较或验证时，请经由 `workflow` 路由，让 manager 拥有 fan-in——上面这个形态就是为此而设的，并不是禁止在没有东西需要合并时使用更简单的模式。

## Workflow on Fleet

预期的高能力路径是由智能体编写的。当主智能体判定一项任务需要的持久协调超过逐回合的子智能体调用时，它会起草一份 Workflow 脚本/IR，按活动权限模式呈现运行计划，运行时再把它编译成带类型的 fleet 工作。

fleet 仍然是子智能体名册与成员选择表面。它拥有成员身份、成员资格、语义角色、已保存的 provider/model 固定项或继承，以及名册状态。Workflow 拥有编排计划：branch、sequence、loop、expand、review 与 reduce 决策。受委派的协调者与 Runtime 拥有槽位准入、启动并发、执行账本，以及每一项权限决策。Workflow 脚本不会直接获得 shell、文件系统、网络、provider 机密、取消或 TUI 权限；worker 在生效的 Runtime 策略下，以 `codewhale exec` 进程的形式执行真实工作。

默认的 Workflow 到 fleet 的校验刻意有界：

- 每次 Workflow 运行最多 1,000 个 worker 智能体；
- 同时最多 16 个存活的 worker 智能体；更大的群体在宿主的每次运行并发门控上排队（阻塞），直到有存活槽释放，然后经 fleet 路由；
- Workflow IR 的结构嵌套不超过 5 层；
- Runtime 子级委派默认 3 层，并有选择加入的硬上限 8 层，与 Workflow 文档的结构深度相互独立；
- 只允许有界循环（必须提供 `max_iterations`）；
- 只允许有界动态扩展（必须提供 `max_children` 加一个模板）。

这些是委派协调的群体上限，不是 fleet 身份，也不是要求一次全部启动。1,000 个智能体的 Workflow 仍应流经已配置的 Runtime worker 池。它们也不是模型步数预算：省略或为零的 `max_steps` 仍然表示无上限。显式的正数 `max_steps` 可以限制该任务，而墙钟超时、取消、provider 保护措施、心跳和准入控制仍各自独立。

推荐的模型布局，例如 DeepSeek Pro 编排器搭配第一层 Flash worker、更外层更便宜的 worker，只是预设。每个槽位都可以继承活动 model，或携带显式的 model 覆盖。继承是字面的：你在 `/model` 中选择的 model 就是 **operator**（`/fleet roster` 中固定的第一行），任何任务规范与名册配置都没有固定 model 的 worker 都会运行在该会话 model 上。一旦选中的成员有了确切的 provider/model 固定项，Runtime 就不会因为某个策略输入不同而悄悄改道该身份；它要么在生效的范围内运行该 route，要么 fail closed。Route 回执记录请求的与解析出的身份。

设置 UI 应把它渲染为一个可展开的网格：一个编排器加上少量可见的子智能体槽位，Right/Enter 下钻到某个槽位的下一层递归 ring，而不是试图一次显示整棵树。

## Task Spec

`codewhale fleet run` 接受 JSON 或 TOML。`codewhale fleet run <spec> --check` 会执行真实运行所做的每一项校验（规范结构、名册成员、智能体配置、模型 route），并打印相同的警告，然后停止：不创建账本、不写入运行、不启动 worker，也不产生任何花费。一个最小 JSON 规范：

```json
{
  "name": "local smoke",
  "tasks": [
    {
      "id": "lint",
      "name": "Lint",
      "instructions": "Run the lint check and report failures.",
      "expected_artifacts": ["log"]
    }
  ]
}
```

worker 是可选的。如果省略，Codewhale 会创建本地 worker 槽位，最多 `--max-workers` 个。

规范文件有三种形态之一，在读取任何字段之前就由其结构决定：

- **文档**——带有 `tasks` 的对象（还可以有 `name`、`labels`、`workers`、`usage_ceiling`）；
- **任务数组**——由任务对象组成的裸 JSON 数组；
- **单个任务**——顶层带有 `id` / `instructions` 的一个任务对象（JSON 或 TOML；TOML 文件绝不会是任务数组）。

数组与单任务文件的运行名取自文件名。由于先选定形态，格式错误的规范会报告真正的问题，例如 ``JSON spec document at tasks[1] (id "review"): missing field `instructions` at line 7 column 5``。已提交的 [`docs/examples/fleet-dogfood.toml`](../examples/fleet-dogfood.toml) 与教程中的 `tasks.json` 都由测试套件解析，因此它们保持有效。

任务规范在 Rust 中带类型，并保持验证数据与 worker transcript 分离。只有 `worker` 的成员/角色引用参与 fleet 身份选择。其余执行字段是成员解析之后才应用的委派协调或 Runtime 输入。一个任务可以声明：

- `id`、`name`、`description`、`objective` 与 `instructions`
- `worker` role、tool profile、tools 与必需 capabilities
- `workspace` 根、必需文件、可写路径与环境 allowlist
- `input_files`、额外的 `context`、`budget`、`timeout_seconds` 与 `retry_policy`
- `expected_artifacts`、`scorer`、`tags` 与自由格式 `metadata`

这些执行策略字段都不会成为 fleet 身份的一部分，也不是另一种成员选择器。省略或为零的 `max_steps` 表示没有模型步数上限；Codewhale 不得凭空合成默认的步数预算。（这是 fleet 文件任务规范的约定；面向模型的 `agent` 调用有所不同——工具解析器会拒绝显式的零。见 `docs/SUBAGENTS.md`。）显式的正数步数限制、超时、取消、provider 保护措施、心跳和准入控制，由受委派的协调者与 Runtime 各自独立地强制执行。

worker 在 `.codewhale/fleet/` 下写有界的 artifact 文件，账本只记录 artifact 引用：kind、path、checksum、MIME type 与 size。回执记录 `pass`、`fail`、`partial`、`skip` 或 `timeout`；失败回执还可能把来源标记为 `transport`、`task` 或 `verifier`。`codewhale fleet status` 会单独呈现这些失败来源的计数。

确定性的内置 scorer 是 `exit_code`、`file_exists`、`regex_match` 与 `json_path`。规范还可以声明 `command`、`code_whale_verifier_prompt` 或 `manual`；这些会记录部分（partial）回执，直到显式的 verifier 通过完成。

### 使用 Role 预设

任务可以引用语义角色名来选择一个唯一的名册成员。内置角色名（`smoke-runner`、`reviewer`、`builder`、`read-only`）为兼容起见仍然可用，也可以在 `[fleet.roles]` 中定义自定义角色。

```json
{
  "name": "smoke check",
  "tasks": [
    {
      "id": "lint",
      "name": "Lint check",
      "instructions": "Run lint and report failures.",
      "worker": { "role": "smoke-runner" },
      "expected_artifacts": ["log"]
    }
  ]
}
```

身份解析之后，兼容性的角色预设可以向受委派的协调者提供工具、超时或重试的默认值。这些默认值不授予权限，不改变选中的是哪个成员，并且仍受 Runtime 收紧。任务规范可以显式请求它的执行设置：

```json
{
  "id": "deep-review",
  "name": "Deep review",
  "instructions": "Review the entire crate for soundness issues.",
  "worker": {
    "role": "reviewer",
    "tools": ["cargo", "rg", "git"],
    "capabilities": ["rust"]
  },
  "input_files": ["crates/**/*.rs"],
  "budget": { "max_tokens": 32000 },
  "expected_artifacts": ["log", "report"],
  "scorer": { "kind": "regex_match", "path": ".codewhale/fleet/report.md", "pattern": "finding|all clear" }
}
```

### 多任务运行示例

一次 fleet 运行可以并行派发几个独立任务：

```json
{
  "name": "CI gate",
  "tasks": [
    {
      "id": "check",
      "name": "Compile check",
      "instructions": "Run cargo check --workspace and report errors.",
      "worker": { "role": "builder" },
      "expected_artifacts": ["log"],
      "scorer": { "kind": "exit_code" }
    },
    {
      "id": "clippy",
      "name": "Clippy lint",
      "instructions": "Run cargo clippy --workspace and report warnings.",
      "worker": { "role": "reviewer", "tools": ["cargo", "cargo-clippy"] },
      "expected_artifacts": ["log"],
      "scorer": { "kind": "exit_code" }
    },
    {
      "id": "security",
      "name": "Secret audit",
      "instructions": "Search for plaintext secrets and report any matches.",
      "worker": { "role": "read-only", "tools": ["rg"] },
      "input_files": ["crates/**/*.rs"],
      "expected_artifacts": ["log", "report"],
      "retry_policy": { "max_attempts": 1 }
    }
  ]
}
```

## 告警

Fleet 告警默认关闭。调用方必须先提供已启用的告警配置，才会发送任何东西。告警 route 匹配带类型的 fleet 事件类别，而不是日志字符串：

- `stale`
- `restart_exhausted`
- `needs_human`
- `budget_exceeded`
- `verifier_failed`
- `run_completed`

Adapter 配置存储环境变量名，而不是机密值。发送时代码从环境或未来的 secrets provider 解析这些名字。账本记录只存储审计标签，如 `slack`、`webhook` 或 `pagerduty`；持久化在账本中的任务规范会脱敏 webhook URL 与路由键。

示例告警配置形状：

```json
{
  "enabled": true,
  "dry_run": true,
  "routes": [
    {
      "events": ["stale", "restart_exhausted", "verifier_failed"],
      "adapter": "ops-slack"
    },
    {
      "events": ["restart_exhausted"],
      "adapter": "pager"
    }
  ],
  "adapters": {
    "ops-slack": {
      "kind": "slack",
      "webhook_env": "CODEWHALE_FLEET_SLACK_WEBHOOK",
      "channel": "#codewhale-fleet"
    },
    "pager": {
      "kind": "pager_duty",
      "routing_key_env": "CODEWHALE_FLEET_PAGERDUTY_ROUTING_KEY",
      "severity": "critical"
    }
  }
}
```

使用 dry-run 检查脱敏后的 adapter payload 而不发送：

```sh
codewhale fleet alert-dry-run \
  --event stale \
  --run-id fleet-demo \
  --worker-id fleet-demo-local-1 \
  --task-id release-triage \
  --reason "worker heartbeat stale since 2026-06-13T02:00:00Z" \
  --adapter slack
```

payload 包含 run id、worker id、task id、status、简短 reason，以及诸如 `codewhale fleet status` 与 `codewhale fleet inspect <worker-id>` 的安全检查命令。端点、webhook 机密与 PagerDuty 路由键显示为 `<redacted:env:...>`。

## 状态表面

`codewhale fleet status` 显示 queued、running、completed、partial、failed、restarted、escalated、cancelled、stale 以及 verifier/transport 失败来源的紧凑计数。`inspect` 显示 worker 状态以及当前任务 objective、role、host、heartbeat、最新事件、artifact 引用、最新错误与告警状态。`logs` 打印有界日志 artifact 内容，`artifacts` 列出 artifact 引用而不内嵌大型 payload。

Runtime API 在现有运行时认证中间件背后暴露同样的由账本支撑的投影：

```text
GET  /v1/fleet/runs
GET  /v1/fleet/runs/{run_id}
GET  /v1/fleet/runs/{run_id}/workers
GET  /v1/fleet/workers/{worker_id}
POST /v1/fleet/workers/{worker_id}/interrupt
POST /v1/fleet/workers/{worker_id}/restart
POST /v1/fleet/runs/{run_id}/stop
```

动作端点调用与 CLI 相同的 manager 控件，并把它们的决策记录在 fleet 账本中。

## Manager 智能体运行手册

Manager 智能体应把 fleet 操作当作带类型的、有账本的控制平面工作。从 `codewhale fleet status` 开始，然后用 `codewhale fleet inspect <worker-id>`、`logs` 与 `artifacts` 检查一次运行或一个 worker。只有当带类型的 CLI/API 表面无法提供所需证据时，才直接读取 `.codewhale/fleet.jsonl`、宿主日志或远程文件。

在采取行动前先对 worker 分类：

- `transient failure`（瞬时失败）：心跳过期、宿主超时、传输被中断、可重试的 provider/网络错误，或一个在不改动任务的情况下合理可能恢复的 adapter 状态。
- `task failure`（任务失败）：worker 完成了，但产生了错误结果、领域失败、缺少必需 artifact，或显式的任务级错误。
- `verifier failure`（verifier 失败）：worker 结果存在，但 scorer/verifier 失败、超时，或与回执不一致。
- `needs-human`：缺少权限、机密请求、破坏性操作、反复的 restart 耗尽、含糊的产品决策，或 manager 无法从带类型 artifact 解决的冲突证据。

选择一个带类型的动作：

- 仅当失败是瞬时的、重试预算还有剩余、任务幂等或可安全重试、且不涉及权限或机密边界时，才重启 worker：`codewhale fleet restart <worker-id>`。
- 仅当当前任务继续下去不安全或操作者明确要求取消时，才中断或停止：`codewhale fleet interrupt <worker-id>` 或 `codewhale fleet stop --all`。
- 默认不要重启纯粹的任务失败；保留 artifact 并把回执交给任务 owner，除非任务规范说明重试可以产生新证据。
- 对于 verifier 失败，先检查 scorer 输入与 artifact 引用。如果无法通过带类型的 fleet 动作修正 verifier，升级给人工审阅。
- 对于 `needs-human`，起草升级内容而不是直接发送，除非告警配置明确授权发送。

安全的 Slack 或 PagerDuty 草稿：

```text
Codewhale fleet needs attention
Run: <run-id>
Worker: <worker-id>
Task: <task-id or unknown>
Classification: <transient failure | task failure | verifier failure | needs-human>
Reason: <one sentence, no secrets>
Latest typed evidence: codewhale fleet inspect <worker-id>; codewhale fleet artifacts <worker-id>
Safe log excerpt: <3 lines max or "see artifact <ref>">
Requested decision: <restart approval | verifier review | task owner review | permission decision>
```

运行后总结应包括 run id、已检查的 workers、分类、已采取或已起草的带类型动作、预期账本影响、已审查的 artifact 引用与下一个 owner。保持总结有界；链接 artifact 引用而不是复制完整日志或 transcript。

捆绑的 `fleet-manager` skill 为 manager 智能体镜像了这本运行手册。它是第一方系统 skill，在系统 skill 安装或刷新后应能通过常规 skill 注册表发现。

## 宿主 Adapter

Runtime 的宿主 adapter 边界支持本地子进程与显式 SSH worker。宿主的选择是 worker 规范上的 Runtime 放置，而不是 fleet 成员身份或成员选择器。它不认证宿主，也不授予访问权限。Adapter 暴露相同的操作：start、read status、read bounded logs、interrupt、restart、stop 与 cleanup。

本地 worker 作为 stdin 关闭、stdout/stderr 写入有界的宿主 adapter 日志的子进程运行。它们只继承一个小的安全基础环境，如 `PATH` 与显式 allowlist 的变量。

SSH worker 通过系统 `ssh` 客户端以 `BatchMode=yes` 与有界连接超时运行。远程环境变量通过 OpenSSH `SendEnv` 发送；值不会嵌入本地 ssh argv 或 fleet 日志。

宿主密钥必须事先受信任：连接使用 `StrictHostKeyChecking=yes`，不会自动接受新密钥。
显式配置 `known_hosts` 文件时，信任仅来自该文件；省略时，OpenSSH 使用其常规宿主密钥库。
请先核实宿主密钥，再加入相应密钥库。旧的 `host_key_fingerprint` 字段不受支持，配置后会被拒绝；
请将它迁移为已核实的 `known_hosts` 条目。`identity` 文件选择客户端登录密钥，并不验证远程宿主。

示例 SSH worker 规范：

```json
{
  "id": "builder-1",
  "name": "Builder 1",
  "host": {
    "kind": "ssh",
    "host": "builder.example.com",
    "user": "codewhale",
    "port": 22,
    "identity": "~/.ssh/codewhale_fleet",
    "known_hosts": "~/.ssh/codewhale_fleet_known_hosts",
    "working_directory": "/srv/codewhale/work",
    "env_allowlist": ["CODEWHALE_PROFILE"],
    "codewhale_binary": "/usr/local/bin/codewhale"
  },
  "capabilities": ["local", "linux", "tests"],
  "max_concurrent_tasks": 1
}
```

默认值刻意保守：

- 不启用托管控制平面或云供给；
- SSH 要求显式的 host、working directory 与 Codewhale 二进制路径；
- 类似机密的环境名，如 `TOKEN`、`SECRET`、`PASSWORD`、`API_KEY` 与 `PRIVATE_KEY`，会被 adapter allowlist 拒绝；
- 机密应留在 Codewhale 配置的 provider 或远程宿主配置中，而不是任务说明、argv 或 fleet 日志里。

## Runtime 策略与权限不属于 fleet 身份

fleet 不定义项目/工作区信任级别、文件系统或网络可达范围、机密访问、approval 模式、沙箱、工具集或执行权限。这些都属于委派协调与 Runtime 策略。这种分离是承重的：

- 成员解析只考虑成员 id/名称、语义角色、provider/model 身份和名册状态；
- 选中的身份在评估任何权限策略之前就已冻结；
- 存在实时父级时，Runtime 应用其上限；独立的 fleet CLI 启动则携带一个显式的、有界的权限范围，两条路径都仍受实时沙箱与平台强制措施约束；
- 任何信任、权限、能力、机密、沙箱、approval 或工具策略的值，都不得选择另一个成员，也不得悄悄改变其 provider/model route；并且
- 回执把请求的与生效的 Runtime 权限档位，同 fleet 成员身份分开报告。

较旧的持久化配置与协议形态里，可能仍含有 `security_policy`、`trust_level`、`permissions`、`capability_grants`、机密引用、宿主认证、环境 allowlist 或工具配置等字段。它们仍可反序列化，用于账本回放，但新的 fleet 运行创建会拒绝 `security_policy` 和 worker 的 `trust_level`，而不是假装它们授予了权限。它们出现在旧数据里，并不会让它们成为 fleet 变量或授权。生效的 Runtime 仍是最终权威，并在请求的操作无法被强制执行时 fail closed。

当前的强制执行行为，请参阅 [Modes](../MODES.md)、[Sub-agents](../SUBAGENTS.md)、[Agent Runtime](../AGENT_RUNTIME.md) 与 [Command Control Plane](./COMMAND_CONTROL_PLANE.md)。请让机密值远离任务说明、参数、日志和回执；adapter 与 Runtime 层必须独立于 fleet 选择，继续对它们脱敏或拒绝。

## 子级授权：0.10.1 的范围与 0.11 的重做（#6298）

子级的权限是一个授权对象 `ChildGrant`（`crates/agent-contract/src/worker_profile.rs`）。它随 v0.10.0（966ef974e1，#5633）发布。它包含 `files`（none / read / write）、`shell`（none / inspect / verify / full）、`network`、`desktop`、工具 `surface`、调用方显式的 `scope`，以及 `spawn`。角色是它之上的预设（`ChildGrant::for_role`），而 `ChildGrant::resolve` 会把预设与由父级导出的配置和调用方的范围取交集。子级的工具目录、它的派发拒绝，以及它的能力范围，读取的都是同一个授权，因此子级看得见的工具就是它能调用的工具。`desktop` 不在任何预设里。

`ShellGrant::Verify` 也随 v0.10.0 发布：Verifier 预设获得有界的内置验证表面（默认的工作区检查、纯粹的测试选择、有界的 Git fetch 与 merge-tree），而不是 shell 语法。

**v0.10.0 发布的其他修复**（建立在该授权之上）：

- 子级从不继承桌面或计算机控制工具（b5e48cd31，#6296）。
- 为 Git 提供有界的验证表面：针对已配置远程名的 `fetch`，以及只读的 `merge_tree`（b89349286）。
- 拒绝信息会指出被认可的替代做法，并告诉子级把被阻止的探测报告给它的父级，而不是绕过它（23747acea）。
- 统一的思考档位词汇（c2bc1244d）。token 预算会被跟踪，但从不强制执行（a7a8bdb33）。

**0.10.1 的范围。** 这个版本不新增授权模型代码。#6298 被重新划定为下面的剩余部分，它们将作为各自独立的切片在 0.11 落地。

**0.11 剩余部分**（一次一个切片）：

1. **面向构建的 `verify` shell 模式。** `cargo test`/`check` 在显式的、有界的写入范围（`target/`、refs）下运行，这样 verifier 无需完整 shell 就能运行交给它的构建。
2. **失败即关闭的工具家族分类。** MCP 与桌面工具构成带标签的家族。只有当 spawn 带着理由授予时，子级才会得到该家族；未分类的工具不会被授予。
3. **可读的授权。** 角色选择器、名册和回执用通俗的话展示生效的授权、model 和思考档位。

相关工作记录在 #6015、#5633、#6194 和 #6232。
