# Fleet 与子智能体

> 英文原文：[SUBAGENTS.md](../SUBAGENTS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-10-02。

Fleet 管理这些子智能体所用的已保存模型和角色分配。单项委派使用 `agent`；带有阶段、依赖和完成检查的工作使用 `workflow`。使用 Fleet 模型候选列表的计划，参见 [Workflow 编写指南](WORKFLOW_AUTHORING.md)。

Fleet 角色是委派工作面向用户的词汇：父智能体通过 `agent` 启动一个专注的 `general`、`explore`、`planner`、`reviewer`、`implement`、`test` 或 `advisor`，`worker` 运行期间会拿回 `agent_id`、声明的交付文件和有效限制。默认回执是紧凑的；需要转录句柄或账本时，再按 ID 请求详情。内部运行时类型是 `FleetRole`（以前叫 `SubAgentType`）；旧的角色拼写（`worker`、`scout`、`plan`、`review`、`builder`、`verifier`、`consultant`、`oracle` 等）在 v0.9.x 期间仅作为持久化/反序列化的兼容适配层继续被接受。新的提示词和配置应使用 Fleet 的规范名称。

子任务与普通回合使用同一个 `Engine::run_turn` 规划器、执行器、Session 和审批收件箱。私有的已捕获 ChildGrant 在最终执行边界收窄工具动作、shell、工作区、网络和后代深度。原有 SubAgentManager 继续管理启动名额、协调、worker 检查点和终态交付，不再运行另一个模型/工具循环。工作回合与唯一的有界汇报回合共享这个已配置的子 Engine 和原始截止时间。

提供商瞬态失败使用 Core 的分发与重试边界。预算耗尽或请求中断会保留已记录的检查点及延续句柄。每次实际分发的请求都携带原始会话、路由和来源标识：提供商用量只计费一次，然后投影到 worker 账本。成功但没有用量的响应与结果未知的请求分别保留覆盖缺口，均不会被标为零成本。需要经受进程重启、休眠或远程执行的工作请使用 Fleet 或由 Workflow 支撑的运行。

子智能体继承父智能体被允许使用的工具注册表，包括 `agent` 协调。生成遵守同一个绝对深度上限：根为深度 0，它的子智能体为深度 1，处于 `max_spawn_depth` 的子智能体不能再生成。操作者默认值为 3，硬上限为 8。角色、已保存的 profile 或兼容请求只能收窄这个上限。恢复和转录分叉会保留来源的位置和限制，不会多换来一代。已移除的 `agent_open`/`agent_eval`/`agent_close` 生命周期工具不会出现在任何注册表中。

健康的子智能体在父智能体的一次普通响应结束后会继续运行。它们的完成通知经由现有的 Engine 收件箱返回，并可唤醒父智能体开始另一个正常回合。显式中断或取消仍然具有最终效力。`detached: true` 另外让一棵子树不受父回合取消的影响；它不会移除子智能体的预算，也不会移除无界面主机的截止时间。

本文档涵盖角色和单个 `worker` 的控制。多项委派请使用 `workflow`，通过同一套 `worker` 运行时来协调；每个角色的提示词见 `crates/tui/src/tools/subagent/mod.rs`（`*_AGENT_INTRO`）和工具的内联描述。

## 角色分类

`agent` 上的 `type` 字段为子智能体选择一种 Fleet 姿态（`agent_type` 作为兼容别名被接受）。每个角色都是对待工作的一种不同立场，而不只是标签不同。

## 维护者姿态

子智能体帮助 Codewhale 更快推进，但维护者的决策仍归父智能体所有。用子智能体收集证据、审查补丁、运行验证，同时保持 [`AGENT_ETHOS.md`](AGENT_ETHOS.md) 中的社区姿态：issue 是开放的接收入口，PR 门禁是审查负载的控制手段，被收割的工作需要明确的贡献者署名。

当子智能体审查社区贡献时，父智能体在合并、收割、关闭或搁置之前，仍应亲自检查 PR diff、关联 issue、测试和 CI。子智能体的结果是一份工作集，不能替代管家式的把关责任。

| 角色 | 姿态 | 可写？ | 联网？ | Shell 姿态 | 典型用途 |
|---|---|---|---|---|---|
| `general` | 灵活；父智能体说什么就做什么 | 是 | 是 | 是 | 默认角色；多步任务 |
| `explore` | 只读；快速摸清相关代码 | 否 | 是 | 有界检查 | "找到 `Foo` 的每个调用点；用 gh 检查这个 PR" |
| `planner` | 分析并产出策略 | 否 | 是 | 只读探针 | "设计迁移方案；不要执行" |
| `reviewer` | 阅读并按严重度打分 | 否 | 是 | 有界检查 | "审计这个 PR 的 bug" |
| `implement` | 以最小改动落地某个具体变更 | 是 | 是 | 是 | "把 `bar.rs::Foo::bar` 重写为做 X" |
| `test` | 运行测试/验证并报告结果 | 否 | 是 | 有界验证（不写入） | "用有界的测试检查来验证这个 diff；报告 PASS/FAIL" |
| `advisor` | 短期、高思考强度的咨询 | 否 | 是 | 无 | "这个设计我们漏掉了什么？" |
| `custom` | 显式的窄工具 allowlist | 继承 | 继承 | 继承 | 在父智能体姿态上精选的工具 |

角色的默认值是该角色*意图*的姿态，而父智能体的有效姿态永远是天花板（子智能体绝不会比父智能体更宽）。只读角色按意图扣留**工作区写入**；默认不会拿走其他任何东西——每个角色都保留网络读取，`custom` 继承父智能体的写入/网络/shell 姿态，并且只会被它的显式工具列表或发起调用收窄。被聚焦的 `worker` 的头部会依据运行时自身的权限快照声明有效姿态（`scout · read-only · network · read-only shell`）。

**委派转移的是工作，绝不是权限。** 只读的父智能体可以委派给 `implement`，但子智能体的有效写入、网络、shell 和工具权限仍在父智能体的实时姿态范围之内。检查类角色可以使用分类过的只读 shell 界面；在具备原生强制隔离的平台上，还可以使用下文所述的显式只读分析模式。换一个角色名，或加一个 `read_only` 标志，都不能授予调用方本来没有的 shell 工具。`fleet/exact.rs` 中的 `ChildAuthority::clamp` 让每个字段都与较窄的一方取交集。拒绝列表取并集，因此 `inherit_disallowed_tools: false` 无法丢掉任何操作者或祖先的拒绝规则。恢复已保存的 `worker` 时，会再次把它保存的姿态与当前调用方的姿态取交集。这一约束由 `crates/tui/src/fleet/exact.rs` 测试中的 `a_read_only_parents_delegation_never_widens_authority` 钉住。

在进程内部，解析后的权限是一个对象——`crates/tui/src/worker_profile.rs` 中的 `ChildGrant`：`files`（none/read/write）、`shell`（none/inspect/verify/full）、`network`、`desktop`（绝不授予子智能体）、具名工具 `surface`、调用方的显式 `scope`，以及剩余的 `spawn` 深度。角色是这个对象上的一组预设（`ChildGrant::for_role`）；`ChildGrant::resolve` 把它与由父智能体派生的 profile 取交集。子智能体的工具目录、分发时的拒绝和能力包络读取的是同一组字段——可见的工具就可以调用，被拒绝的工具永远不会出现。

会话的**权限姿态**在每个子智能体内部的适用方式，与它在父回合上完全一致：在 Auto-Review 下，同一个确定性底线和一次性的模型守护者来裁决 `worker` 被扣留的调用（从不弹提示；守护者不可用时拒绝，即 fail closed）；在 Ask 下，角色无法代为决定的被扣留调用，会作为审批提示出现在父智能体的 UI 中，`worker` 显式地等待（`waiting for user`），在无法弹提示的主机上则带着原因被拒绝；Full Access 与父回合完全一致地生效，用户已授予的调用不会再被 Auto-Review 底线拦下。每一次无人被提示的裁决，都是该 `worker` 转录中的一行备注（聚焦它时可见）和一条审计日志记录。参见 `docs/MODES.md`。

每个角色完整的系统提示词位于 `crates/tui/src/tools/subagent/mod.rs`（搜索 `*_AGENT_INTRO`）。提示词前缀在子智能体启动时自动加载；父智能体的委派提示词成为第一个回合的用户消息。

## 上下文分叉

`agent` 默认全新启动：子智能体拿到它的角色提示词，加上你传入的任务。当子智能体应从父智能体当前的请求前缀继续时，请使用 `fork_context: true`。（`fork_context` 不在对外公布的 schema 中——它对兼容调用方仍可解析，只读角色的自动分叉也保持不变。）在分叉模式下，运行时会尽可能让父智能体的 prefill/提示词前缀逐字节一致，追加一份结构化的状态快照，然后在末尾加上子智能体的角色指令和任务。这样既保留了 DeepSeek 前缀缓存的复用，又给了子智能体做延续、审查、总结或压缩工作所需的上下文。

独立的探索使用全新会话。当任务依赖父智能体转录中已有的决策、文件、todo 或 plan 状态时，使用分叉会话。

分叉状态显示的是父智能体的 To-do 快照——由 `todo_write` 写入的唯一的 Work 界面。子智能体的 `<codewhale:fork_state>` 块携带 `crates/tui/src/todo_snapshot.rs` 渲染的有界正文，因此分叉是从父智能体真实的进度位置继续，而不是从一段转述继续。这一 To-do 部分在生成发生时解析，所以同一个父回合中更早的 `todo_write` 也会被包含进来。

**该列表只在生成的那一刻显示一次，之后绝不会重新发送。** 没有哪个子智能体请求会重新陈述 To-do 列表，父智能体的请求也不会。每个智能体都保留自己的私有列表（#4810）；它对这个列表的了解，来自它自己的 `todo_write` 调用（每次调用都发送完整列表）以及这些调用返回的简短回执，而这些都是它自己转录中的普通消息。因此 `worker` 无法读取或写入父智能体或兄弟智能体的列表，被分叉出的子智能体也无法修改交给它的快照，或持续读取父智能体之后的变化。

子智能体转录内卡片显示的正是这份私有列表。一张委派卡片渲染的是**它自己**那个智能体 To-do 的有界投影——已完成/总数计数、始终包含进行中的条目、最多三行，以及当界限省略其余部分时显式的 `… +N more`——由 `card_todo_projection` 用与面向模型的正文相同的快照、优先级顺序和清洗器构建。卡片只会消费 `agent_id` 与自己匹配的信封，因此父智能体的列表绝不会出现在子智能体名下，也没有兄弟智能体的列表会出现在另一个名下。没有陈述任何工作的智能体完全不显示 To-do 行，而不是显示占位任务；终态卡片保留它的智能体实际发布的最后一份快照。扇出（fanout）卡片保持为圆点网格，不显示子智能体的 To-do：一张卡片背后有许多 `worker` 时，没有一个能如实挂载单一列表的位置。只有当运行时已经把该子智能体表示为它自己的委派卡片时，才会出现子智能体的 To-do。

持久化的 Runtime 账本（通过 Fleet 任务状态投影）仍然拥有生命周期状态。模型已经无法触达 `update_plan`：`model_visible()` 返回 `false`（`crates/tui/src/tools/plan.rs:408-413`），因此它会从 API 工具列表中被过滤掉，永远不会出现在子智能体面前。它只为重放更早的转录而保留。以前放在那里的策略，现在放进响应正文；生命周期状态则放进 `todo_write`。

## Worktree 隔离

对于并行的编辑通道，用 `worktree: true` 启动子智能体。Codewhale 会为该子智能体创建一个全新的 git worktree 和分支，从隔离的检出中运行它，并在返回的会话投影和 `worker` 记录里报告得到的 workspace/分支。默认分支是 `codex/agent-<name>-<id>`，检出位于父仓库旁边的 `.codewhale-worktrees/` 之下，因此父检出保持干净。

隔离不等于写入权限。没有指定角色/profile 或写入声明的纯提示词启动仍保持只读，而只读角色不需要写入作用域。显式选择的可写角色（如 `general` 和 `implement`）继承父智能体的写入上限，并在没有被收窄时默认作用于工作区（`write_roots: ["."]`）。并行工作应优先声明显式且互不相交的 `exact_files` 或 `write_roots`；`coordination_contracts` 可以预留具名的共享契约。如果写入者的作用域只由 `deliverables` 提供，这些文件就成为精确文件作用域。

`write_authority` 是可选的类型化收窄：`read_only` 不接受任何写入作用域，`workspace_write` 使用共享检出，`worktree_write` 要求实际的 worktree 隔离。不兼容的角色/作用域声明会在准入之前失败。处于活跃状态、相互重叠的共享声明会在变更之前失败；真正隔离的 worktree 可以并行进行。`custom` 角色需要显式的、具备写入能力的权限才能申领写入；否则它以只读方式启动。

只读不等于只能读文件。一个通常具备写入能力、被 `write_authority: "read_only"` 收窄的智能体，只要其父智能体允许，就仍保有现有的、由分类器界定的检查 shell：Git 历史/状态、搜索，以及被允许的 `gh` 日志读取，而不是任意命令或测试程序。Workflow 的只读步骤同样使用有效角色的工具，而不是再强加一份仅限文件的列表；`test` 角色保留其有界的验证接口。显式的工具 allowlist、`deny_all_tools`、父智能体的拒绝、网络限制和变更检查依然适用。没有 shell 访问权的父智能体无法委派它。

可选字段：

- `worktree_branch`：要创建的确切分支。
- `worktree_base`：要从中开出分支的 git ref；默认为 `HEAD`。
- `worktree_path`：确切的检出路径。相对路径和绝对路径都必须位于默认的同级 `.codewhale-worktrees/<repo>/` 根目录之下（检查之前会先解析符号链接）。任何 worktree 请求都保留审批卡片，即使是只读角色。

`cwd` 可以与 `worktree` 组合：所请求的目录成为发现锚点，仓库根（以及新的检出）由此解析（`prepare_child_workspace`）。没有 `worktree` 时，`cwd` 仍是对父工作区内已创建目录的手动逃生舱。

### 文件交付物与编辑声明

把必需的文件放进 `deliverables`；把面向人的成果放在 `expected_artifact`。例如，这样调用 `agent`：

```json
{
  "action": "start",
  "type": "implement",
  "prompt": "Summarize the local routing evidence in reports/routing.md.",
  "exact_files": ["reports/routing.md"],
  "deliverables": ["reports/routing.md"],
  "expected_artifact": "A concise report with source references and open gaps"
}
```

最多接受 16 个仓库相对的文件路径。绝对路径、路径穿越、仓库元数据路径和符号链接穿越都会被拒绝。完成时会对照已准入的作用域检查每个文件，并在可用时报告它的路径、状态和字节数。终态状态有 `present`、`missing`、`empty`、`not_file`、`out_of_scope`、`invalid_path` 和 `unreadable`。`present` 表示存在一个非空的常规文件；它不能证明报告是正确的，也不能证明测试通过了。

缺失或无效的必需文件会把 `verification.status` 设为 `deliverable_missing`，并附上各文件的判定。成功的文件检查可以产生 `deliverables_present`；它们不会把子智能体的自我报告变成独立的质量关卡。完成通知包含实际的判定，包括 `worker` 失败或耗尽预算的情形。

编辑声明会针对生成时的 git HEAD 和脏文件内容单独检查。显式的改动文件声明，在被声明的文件实际没有改动，或一次成功的有界写入回执改动了子智能体没有声明的文件时，可以产生 `claim_mismatch`。同级智能体在某个 `worker` 宽泛作用域内的改动，不足以把那次写入归到该 `worker` 头上。`path:LINE` 和 `path:LINE-LINE` 形式的证据引用（包括带句末标点和 Markdown 链接的写法）永远不算编辑声明。

### 只读 shell 命令

Scout、reviewer 和 planner 智能体、被 `write_authority: "read_only"` 收窄的智能体，以及拥有只读 shell 授权的持久化 Fleet `worker`，都用同一套只读语法来裁决 `bash` 调用：

- 检查类程序：`ls`、`pwd`、`cat`、`head`、`tail`、`wc`、`which`、`stat`、`file`、`du`、`df`、`grep`、`rg`、`fd`、不带 `-exec` 或 `-delete` 的 `find`，以及 `sed -n <range>p`；
- `git status`、`log`、`diff`、`show`、`ls-files`、`blame` 和 `grep`，可以在其前面带 `-C <dir>` 或 `--no-pager`；
- 文本过滤器 `sort`、`uniq`、`cut`、`tr` 和 `comm`，以及字面量的 `echo`/`printf`；
- 有网络授权时，`gh` 的 issue/pr/release/repo/run/workflow 的 view 或 list 读取。

即使有网络授权，`npm view` 也会被拒绝：npm 配置可以选择可执行的辅助程序，因此包元数据读取不在这套有界语法之内。

被准入的命令可以用 `|`、`&&`、`||` 和 `;` 连接，例如 `git diff HEAD && echo '=== FILES ===' && ls -la`。开头的 `cd <dir> &&` 用来设置工作目录，该目录必须在工作区之内。唯一允许的重定向是 `2>/dev/null`、`>/dev/null` 和 `2>&1`。引号内的文本是数据，所以 `rg 'a && b' src` 是一次搜索。其他重定向、`$` 或反引号展开、子 shell、后台运行、行内环境变量赋值，以及任何其他程序（如 `python`、`awk`、`jq` 或 `cargo`）都会被拒绝。选项和路径操作数仍会被检查，并且每一次 `gh` 或 `npm` 读取，无论出现在命令的哪个位置，都需要网络授权。

被拒绝的命令会作为一个点出所违反规则的错误结果返回给智能体，例如 `[shell.readonly.command] program: `touch` is not a read-only inspection command`，后面跟着智能体可以改做什么。智能体继续工作，而没有进展的反复拒绝会让它以失败而不是完成告终。

### 在写入者旁边读取

当某个同级智能体持有共享写入声明时，只读工具和经分类器批准的 shell 读取仍然可以运行。对于任意的分析代码，请用显式的 `read_only: true` 调用 `bash`：

```json
{
  "action": "run",
  "read_only": true,
  "command": "python3 -c \"import sqlite3; db = sqlite3.connect('file:cache/index.db?mode=ro', uri=True); print(db.execute('SELECT name FROM sqlite_schema').fetchall())\""
}
```

这一模式要求原生的文件系统只读隔离，并且拒绝网络访问。它只接受前台的 `run`，以及 `command`、可选的 `cwd` 和 `timeout_ms`。后台或交互模式、stdin、沙箱升级以及外部执行后端都与它不兼容。如果原生强制隔离不存在或无法准备，调用会在执行命令之前拒绝；这个标志绝不会退回去相信一句“代码只读不写”的承诺。现有的角色、工具和祖先策略限制依然适用。

对于自己作用域之外的写入拒绝，`agent(action="claim", ...)` 可以把被允许的路径加入你的声明，但它不能夺走仍然存活的同级智能体的声明。请等待该同级智能体，选择互不相交的有界写入，或对需要写入的代码使用单独的 worktree。`action="release"` 只会清除所有者已不再存活的声明；它不是用来解锁另一个正在运行的 `worker` 的文件的手段。

## 委派简报

父智能体应当传递一份紧凑的简报，而不是一段松散的文字。用结构化的 `dependencies` 和 `acceptance` 数组承载有界的前提事实和可观察的检查；把聚焦的目标放在 `prompt` 中。不要复制父智能体的原始推理，或一段无界的转录。

简报的默认形态——委派很简单时，一句朴素的话胜过这个模板：

```
QUESTION:
SCOPE:
ALREADY_KNOWN:
EFFORT: quick | medium | thorough
STOP_CONDITION:
OUTPUT: VERDICT, EVIDENCE, GAPS, NEXT
```

`scout` 简报默认为快速的只读调查（不写入，但网络触达和有界验证界面可用于真正的侦察）。一次快速侦察通常只需要寥寥几次调用：定位、搜索、读取决定性的那几行，然后返回——但要在拿到决定性证据时停下，而不是在达到某个数字时停下。这里没有需要节省的“每个智能体调用上限”；运行时限制的是深度和并发，而不是好奇心。除非证据与之矛盾，否则不要重复 `ALREADY_KNOWN` 中的工作。Builder 和修复类简报应当在扩大范围之前，或在反复失败之后设置检查点。

好的委派提示词示例：

```text
QUESTION: Does PR #N introduce release-risk behavior around provider routing?
SCOPE: PR #N diff, linked issue, provider routing tests, docs/PROVIDERS.md.
ALREADY_KNOWN: Branch is <release-branch>; workspace version is <live-version>.
EFFORT: medium
STOP_CONDITION: Return once you have either one BLOCKER/MAJOR issue or enough evidence for no MAJOR+ issues.
OUTPUT: VERDICT, EVIDENCE with file:line refs or PR refs, GAPS, NEXT.
```

```text
QUESTION: Where is the child-agent prompt assembled?
SCOPE: crates/tui/src/prompts*, crates/tui/src/tools/subagent/*.
ALREADY_KNOWN: The model-facing launcher is only `agent`; do not look for removed lifecycle tools.
EFFORT: quick
STOP_CONDITION: Stop after identifying the prompt source files and the function that wraps assignment text.
OUTPUT: VERDICT, EVIDENCE, GAPS, NEXT.
```

```text
QUESTION: Is the focused prompt/subagent test filter valid, and what fails if not?
SCOPE: cargo test -p codewhale-tui --lib --locked prompt; subagent filter if needed.
ALREADY_KNOWN: Do not fix failures; capture exact command, exit code, and first relevant assertion.
EFFORT: medium
STOP_CONDITION: Stop after one clean PASS or one reproducible failing assertion with command evidence.
OUTPUT: VERDICT, EVIDENCE, GAPS, NEXT.
```

（简报字段名和上面的示例是发给模型的协议文本，保持英文原样。）

### 何时选择哪个角色

- **`general`** —— 当任务是“把这整件事做完”，而不是“去看看”“去设计”或“去验证”。这是正确的默认；只有当姿态重要时，才改用更具体的角色。
- **`explore`** —— 当父智能体在决定下一步之前需要证据。Scout 便宜又快；对相互独立的区域可以并行开 2–3 个。它们应当先定位：确认项目根目录，在不熟悉的目录树中阅读相关的 `AGENTS.md`/`README.md` 指引，只搜索可能的范围，并返回 `path:line-range` 形式的证据，而不是一篇叙述式的导览。要使用的角色名是 `explore`。
- **`planner`** —— 当父智能体有目标但没有可执行的分解。Planner 写工件（`todo_write` 条目、响应正文里的策略），但不去执行它们。
- **`reviewer`** —— 当已经有一个变更，父智能体想让它被评分。Reviewer 在只读姿态下运行，所以运行时会拒绝补丁尝试——请在发现中描述修复方案，当结论是“修它”时由父智能体派出一个 builder。
- **`implement`** —— 当变更已经明确，只需要落地。Builder 保持严格的范围：最小改动，不顺手重构，交回之前先跑一次快速验证。
- **`test`** —— 当父智能体需要对测试套件或其他验证给出权威的通过/失败结论。Verifier 姿态永不写入——运行时会拒绝修复尝试——所以请记录失败的断言和栈，把修复候选放在 RISKS 之下，交由父智能体去派发。Shell 被收窄到有界的内置验证界面：Run tests/verifiers（检查位于子目录时传 `cwd`）、用于远端引用的 Git fetch、用于合并结果的 Git merge_tree。写入上限是只读，无界的 shell 形式会被拒绝（#5186）。被拒绝的探测会上报给父智能体，绝不绕行（#6298）。
- **`advisor`** —— 当操作者希望在更便宜的执行继续之前，得到一个高杠杆的第二意见。Consultant 会读足够的材料来支撑一条建议；它们的授权不包含写入，也不包含 shell，运行时会拒绝这两者。`oracle` 和 `consultant` 只在加载较旧的请求或持久化记录时仍被接受；新的提示词、回执和 UI 使用 `advisor`。
- **`custom`** —— 只有当父智能体需要显式约束工具集时才用。通过旧版/内部子智能体记录上的 `allowed_tools` 字段传入 allowlist；面向模型的 `agent` 工具刻意让公开 schema 保持很小。

### 别名

模型可以用多种写法拼出每个角色：

| 规范名 | 别名 |
|---|---|
| `general` | `worker`、`default`、`general-purpose`、`general_purpose` |
| `explore` | `scout`、`explorer`、`exploration` |
| `planner` | `plan`、`planning`、`awaiter` |
| `reviewer` | `review`、`code-review`、`code_review` |
| `implement` | `builder`、`implementer`、`implementation` |
| `test` | `verifier`、`verify`、`verification`、`validator`、`tester` |
| `advisor` | `consultant`、`oracle`（仅作兼容输入） |
| `custom` | （无；需要显式的 `allowed_tools` 数组） |

所有匹配都不区分大小写。未知值会产生一个类型化的错误，列出可接受的取值，这样模型可以在下一回合自我纠正。

## 并发上限

默认最多有 **64** 个子智能体并发运行（`DEFAULT_MAX_SUBAGENTS`），可通过 `~/.codewhale/config.toml` 中的 `[subagents].max_concurrent` 配置，最高不超过 **128** 的硬上限（`MAX_SUBAGENTS`）。会话默认准入一个有界队列，运行中加排队中的子智能体最多 **1024** 个（`MAX_SUBAGENT_ADMISSION`，`crates/tui/src/config/subagent_limits.rs:21`），因此一个回合可以请求广泛的扇出，让管理器逐步消化，而不会造成无界的数量。

默认情况下，每个被准入的子智能体都可以立即启动——除了下面描述的速率限制调节器之外，没有任何人为的节流。请按工作实际需要的扇出来请求，让运行时排队并逐步消化；上面的上限是强制执行，不是预先拒绝合理工作的理由。如果想要更温和的扇出，请调低 `[subagents].launch_concurrency`（一次启动多少个直接子智能体）；超出该限制的子智能体会**排队**等待启动槽位，而不是一拥而上。`launch_concurrency` 默认为解析后的 `max_subagents` 上限。（v0.8.61 之前的 `interactive_max_launch` 键仍作为已弃用的别名被接受；两者同时设置时，新键生效。）

高扇出的 Workflow 可以用 `[subagents] max_admitted`（别名：`max_total`、`admission_limit`）调节这个有界的数量。该总量上限同时计入**运行中**和**排队中**的智能体，而 `launch_concurrency` 让瞬时的执行数量保持有界。已完成/失败/已取消的记录会保留供检查，但不占用准入槽位。丢失了 `task_handle` 的智能体（例如跨进程重启之后）同样不计入该上限。

### 速率限制调节器

唯一的自动节流是速率限制调节器。它在 60 秒的窗口内观察提供商的速率限制（HTTP 429）。反复触发限制之后，它会减少启动槽位的数量；在持续的突发之下，它会完全暂停新的启动。持续的成功会一次一个地把槽位加回来。它绝不会打断已经在运行的智能体，配额耗尽也不被视为节流。

当调节器压住启动时，它会在两个地方说明：

- 排队中的智能体所在行会给出原因，例如 `launch slots throttled to 4/8 after 2 provider rate limit(s) in the last 60s` 或 `launches paused after 4 provider rate limit(s) in the last 60s`，以及它的墙钟预算结束的时间；
- `GET /v1/agent-runs` 会在 `runs` 旁边返回一个 `governor` 对象，包含 `launch_slots`、`max_launch_slots`、`paused`、`recent_rate_limits`，以及启动被压住期间的一行 `status`。它描述的是处理该请求的运行时所发起的启动（Fleet 运行）。

已知限制：`/subagents` 登记表头目前还不显示调节器那一行；TUI 从 Engine 收到的智能体列表不带调节器状态。

提供商 profile 可以让一份配置对直连 API 路由保持激进，同时对订阅式或聚合路由保持温和。`[subagents.providers.<provider>]` 下的每个键在省略时都继承 `[subagents]`。提供商键接受规范名称（如 `deepseek`、`zai`、`openrouter`）以及别名（如 Z.ai 的 `glm`）：

```toml
[subagents]
# Global fallback for providers without a profile.
max_concurrent = 20
launch_concurrency = 20
max_admitted = 200
# Operator-selected Runtime delegation depth. The default is 3; this explicit
# value opts in above the default but remains below the hard ceiling of 8.
max_depth = 6
# Omitted or zero model-step budget is unbounded. Set a positive value only
# when an operator deliberately wants a per-child cap.
default_max_steps = 0
default_wall_time_secs = 1800

[subagents.providers.deepseek]
# Direct API key with room to fan out.
max_concurrent = 20
launch_concurrency = 20
max_admitted = 200

[subagents.providers.glm]
# Z.ai / GLM subscription-style route: keep pressure tight.
max_concurrent = 4
launch_concurrency = 3
max_admitted = 12
max_depth = 2
api_timeout_secs = 180
heartbeat_timeout_secs = 240

[subagents.providers.openrouter]
max_concurrent = 5
launch_concurrency = 3
max_admitted = 20

[subagents.providers.anthropic]
max_concurrent = 3
launch_concurrency = 2
max_admitted = 12
```

用 `/config subagents status` 可以同时看到全局值和当前提供商解析后的扇出、深度和超时 profile。

## 对外公布的 agent 工具字段

面向模型的 `agent` schema 公开以下控制项：

| 用途 | 字段 |
| --- | --- |
| 启动与路由 | `action`、`prompt`、`type`、`profile`、`name`、`model`、`model_strength`、`thinking` |
| 作用域与产出 | `worktree`、`cwd`、`write_authority`、`write_roots`、`exact_files`、`coordination_contracts`、`deliverables`、`expected_artifact` |
| 收窄运行限制 | `max_steps`、`wall_time_secs` |
| 协调与恢复 | `agent_id`、`agent_ids`、`all_parked`、`message`、`until`、`detached`、`resume_from` |
| 检查 | `detail`、`offset`、`limit` |

`start` 需要 `prompt`。`message` 需要目标和消息；`followup` 需要一条消息，以及恰好一种目标形式：`agent_id`/`name`、`agent_ids` 或 `all_parked: true`。`peek`、`interrupt` 和 `cancel` 需要目标。`claim` 需要作用域条目。这些 action 的要求会在执行之前校验。

`agent(action="roster")` 报告每个内置角色解析后的提供商、模型、思考强度、已知的路由限制和能力来源。它与执行使用同一个解析器。显式保存的 profile 优先，其次是当前配置中手动的角色固定选择，再其次是唯一固定该语义角色的已保存成员。相互冲突的任务 `model` 或 `model_strength` 选择会在准入之前失败。对于未固定的角色，按任务的 `model` 优先于 `model_strength`，然后是继承的角色默认值和会话路由。选中 Pod 时，`models` 行按保存顺序列出它的确切路由。对未固定角色的任务，请使用列出的 `provider/model` 选择器；会话模型仍被允许。不在列表中的选择会失败并附上允许的路由；被多个提供商共用的裸模型名需要确切的选择器。没有选中模型时，当前提供商的覆盖和 `model_strength` 保持原有行为；来自其他提供商的请求会失败。这些选择不会改变子智能体的权限。

`profiles` 行展示来自已选定的 Fleet，或受信任的配置/个人/工作区/插件层里已保存的成员，带有有界的身份和同样的路由/成本证据。`profile="bug-hunter"` 会加载该成员的指令、角色、提供商/模型固定选择和深度限制。冲突的 type 或 model 请求会被拒绝；显式的 `thinking` 会覆盖保存的层级。缺失的提供商、被撤销的插件权限和被禁用的项目 profile，都会在子智能体准入之前失败。发现（discovery）绝不会创建 profile 或登记模型。这些身份选择使用现有的子智能体生命周期；仅有一个已保存的 profile，并不会创建持续的 Bot 对话或计算机租约。

成本类别描述的是当前未缓存的文本输入/输出费率，而不是未来某个任务的总价。缺失的或取决于路由的价格保持未知；订阅式/本地路由标注为不按金额计量。发现不会向提供商发出任何请求，并将可达性报告为未验证。

**可解析但不公布（兼容）。** 其他输入仍被接受，用于已保存的转录、ACP/MCP 客户端、Fleet 执行数据，以及内部/操作者的兼容。运行时会校验它们，并与实时策略取交集：

- 委派兼容：`max_depth`、`maxDepth` 或 `max_spawn_depth`；取值限制在 0 到 Runtime 硬上限 8 之间，并且只会收窄继承来的绝对上限。面向模型的调用从操作者和所选 profile 继承深度。
- 工作区/隔离：`workspace_policy`、`fork_context`、`worktree_path`、`worktree_branch`、`worktree_base`
- 生成契约：`deliberate`、`dependencies`、`acceptance`、`allowed_tools`
- 生命周期附加项：`timeout_secs`（wait）、`reason`（interrupt）、`include_archived`（status）

兼容输入不是用来放宽继承来的权限，或去掉有限预算的途径。

## 子智能体预算（步数、墙钟时间、token）

`max_steps` 和 `wall_time_secs` 是可选的逐次调用限制。它们各自只能收窄适用的角色、操作者、父智能体和已保存运行的限制；唯一的例外是 `wall_time_secs` 可以抬高内置的 1800 秒默认值（见下文）。省略即继承这些限制；显式的零、null、负值或越界值会被工具解析器拒绝（schema 最小值为 1）。Fleet 文件任务规格使用不同的约定——那里省略或为零表示无界；参见 `docs/FLEET.md`。

`max_steps` 统计模型回合，接受 1 到 2000。所有角色默认没有模型回合上限，除非操作者或祖先提供了一个；该默认值内部用零来表示，它绝不会取消一个有限的继承上限。`wall_time_secs` 接受 1 到 86400。省略时默认为 1800 秒。为了长时间无人值守的工作，显式值可以高于这个内置默认值；操作者配置的 `default_wall_time_secs` 既是默认值，也是上限，而角色、父智能体和已保存运行的截止时间仍然只会收窄。它同时覆盖模型请求和工具。有效的绝对截止时间会被持久化，并在排队的智能体启动时再次保存，因此之后的续跑受该智能体实际遵循的截止时间约束。

工作时钟从智能体拿到启动槽位时开始。在启动队列里等待的智能体最多等待 `wall_time_secs`；如果这段时间内没有槽位空出来，它会以 `never started` 的原因和零步失败，而不是被报告为用尽了预算的运行。等待之后确实拿到槽位的智能体，从那一刻起获得完整的 `wall_time_secs`，仍受任何父智能体、已保存运行或来源截止时间的约束。因此，父智能体对一个排队的智能体最多可能要等大约两倍的 `wall_time_secs`。排队的那一行会写明等待的原因以及智能体停止等待的时间。如果智能体经常等很久，请一次少启动几个。

例如，一次聚焦的审查可以这样请求：

```json
{
  "action": "start",
  "type": "reviewer",
  "prompt": "Review the parser diff and report concrete regressions.",
  "max_steps": 12,
  "wall_time_secs": 300
}
```

回执中的 `effective_limits` 才是权威；请求 300 秒并不能延长父智能体更早的截止时间。续跑会保留来源剩余的步数、原始截止时间和 token 历史。新的 ID、角色或 `resume_from` 分叉都无法重置这些界限。

### Token 记账与部分结果

Token 预算已在 0.9.14 中退役：token 用量会被追踪，但绝不强制执行——运行不再因为 token 记账而被停止。仍带有 `token_budget` 的旧输入可以被解析，但会被忽略；`max_steps` 和 `wall_time_secs` 仍是可收窄的逐次调用限制。

调节器使用提供商报告的输入加输出 token，而不是把本地估算当作账单。请求的输出会被限制在剩余额度之内。未知的提示词用量和已经在途的请求可能超支；回执会保留完整的报告用量。缺失的用量保持未知。`worker` 记录会区分该 `worker` 自己的 token 总量，与共享的 `budget_spent_tokens` 和 `budget_remaining_tokens`；不要为每个后代都把共享池累加一次。

`worker` 会在这些限制内预留出写一份最终报告的余量：token 额度的至多 10%（最多 8192 个 token，且仅当至少能预留 1024 时），在步数上限允许至少两步时预留一个回合，以及墙钟时间的至多 10%（最多 10 秒）。普通的任务执行会在用到这份预留之前停下。共享作用域会为该作用域扣留一份 token 预留；正在汇报的 `worker` 以原子方式认领剩余余量，使同级 `worker` 无法各自重复使用它。续跑绝不会退还已测得的用量，也不会重置原始截止时间。

最终汇报回合使用该 `worker` 已解析好的提供商和模型，禁用工具，输出最多 1024 个 token。它把有界的助手备注和工具结果整合为发现、证据、产出的文件、未完成的工作和下一步。估算的输入成本计入它的额度。提供商传输层的重试保留在这一个逻辑回合及其原始墙钟截止时间之内；不会额外增加 `worker` 摘要的重试循环。Token 估算不是计费回执：未知的提供商输入和已经在途的请求仍可能超支，实际用量会被记录。

结果仍是 `BudgetExhausted`，即使拿到了有用的报告也是如此，并附带具体原因、检查点、已测得的用量和正常的交付物判定。如果额度太小或已经用完、更早的有界用量未知、提供商失败或时间到期，`worker` 会返回已记录的部分文本，并说明为什么无法得到模型报告。已知缺失的响应用量，以及被超时或取消打断的尝试，会在续跑和共享的同级之间保持记录；之后已知的用量仍然只是一个小计，无法在该有界作用域内恢复汇报余量。取消优先于汇报。缺失的用量保持未知。已耗尽的作用域会拒绝进一步的生成或续跑；部分报告不算成功完成。

## 各角色模型（#3018）

子智能体可以运行在与父智能体不同的模型上。结构化的角色固定选择、旧版模型映射和便捷键，共同汇入一份覆盖映射。结构化的 `[subagents.roles.<role>]` 条目优先于 `[subagents.models]`，后者又优先于便捷键。键不区分大小写；在结构化表内，规范角色键优先于它的旧别名：

```toml
[subagents]
default_model  = "deepseek-v4-flash"   # fallback for every role
worker_model   = "deepseek-v4-pro"     # worker
scout_model    = "deepseek-v4-flash"   # scout
planner_model  = "deepseek-v4-flash"   # planner
reviewer_model = "deepseek-v4-pro"     # reviewer
custom_model   = "deepseek-v4-pro"     # custom

[subagents.models]
# Free-form role → model map; any role alias accepted by agent works.
builder = "deepseek-v4-pro"

[subagents.roles.reviewer]
model = "deepseek/deepseek-v4-pro"
```

这些是针对直接启动和 Workflow `agent` 启动的手动固定选择。任务可以重申同一个模型或确切的 provider/model 组合，但不能用 `model` 或 `model_strength` 改变这个固定选择。显式保存的 profile 优先于手动的角色固定选择。在没有手动覆盖时，只指定类型的启动也会选中唯一的已保存角色固定选择；有歧义的已保存角色会失败，而不是随便选一个。持久化的 Fleet 运行保留所选成员冻结下来的路由。

结构化的角色固定选择可以列出获准的替换路由：

```toml
[subagents.roles.reviewer]
model = "xai/grok-4.6"
replacements = ["deepseek/deepseek-v4-pro"]
```

当固定的路由拒绝了智能体的**第一次**请求（配额耗尽、凭据或授权被拒、或模型不可用）时，智能体会在下一条列出的路由上重试同一个请求，并保持它的角色、权限、工具、作用域和预算。列出一条路由，就等于授权把该智能体的任务发送给那个提供商，因此每个条目都必须写明 `provider/model`；最多允许三个，每个只尝试一次。路由回执会记录有效路由、`route_source = "role.replacement"`，以及一条包含原路由、原因和尝试次数的备注。在智能体已经运行过工具之后，绝不会发生替换；对内容策略、上下文长度或无效请求类错误，绝不会替换；对 Codewhale 自己的权限拒绝，绝不会替换；对确切的 Fleet 成员或任务级 `model` 选择，同样绝不会替换，它们始终保持确切。

结构化的角色固定选择接受 `provider/model`，保留所配置提供商的确切身份和完整的模型后缀。未知的提供商、空的组合，以及跨提供商的 `auto` 选择会在准入之前失败。裸的结构化模型继承会话的提供商。对于带命名空间的模型，请显式限定，例如 `openrouter/deepseek/deepseek-v4-pro`。旧版的标量值和 `[subagents.models]` 的值保留由提供商拥有的完整 id（包括斜杠）；它们不会更换提供商。

v0.9.x 的便捷键 `explorer_model`、`awaiter_model` 和 `review_model` 仍作为已弃用的别名被接受，这样现有的配置文件不会失效。

模型 id 可以是**当前提供商接受的任何模型**——校验是提供商感知的，发生在生成时，而不是加载时。在官方 DeepSeek API 上，只接受 DeepSeek 的 id；其他所有提供商都会把 id 原样交给提供商 API，由它来裁决。一个非 DeepSeek 的示例：

```toml
provider = "moonshot"
model = "kimi-k2.7-code"

[subagents]
worker_model = "kimi-k2.6"
```

模型 id 应用到子智能体路由时以同样方式校验；官方 DeepSeek API 上无效的 id 会让生成失败，并附上可接受 id 的列表，而不是一个晦涩的提供商 400 错误。

在 `/model auto` 下，子智能体路由同样是提供商感知的：有已知大/小（便宜）配对的提供商（DeepSeek，以及 NVIDIA NIM、OpenRouter、Novita、SiliconFlow、SGLang、vLLM 上托管的 DeepSeek 路由）在这一配对之间路由；没有已知便宜档的提供商（如 Ollama、Moonshot）则跳过网络路由器，让子智能体留在会话模型上。

## 各 profile 的提供商路由（#3965）

`[subagents.models]` 在当前提供商内部更换子智能体的模型。该旧版输入里的斜杠并不会授予另一个提供商。要把子智能体固定到不同的提供商，请像上文那样使用结构化的 `[subagents.roles.<role>]` 声明，或使用 Fleet/AgentProfile，并通过 `profile` 或它唯一的已保存角色来选择它。profile 显式的 `provider` + `model` 字段优先于父会话的路由；省略 `provider` 则保留现有的继承行为。

示例：让父会话留在 DeepSeek，但让一个格式化子智能体运行在本地 LM Studio 的 OpenAI 兼容端点上：

```toml
# ~/.codewhale/config.toml or workspace config
provider = "deepseek"

[providers.deepseek]
api_key = "YOUR_DEEPSEEK_KEY"

[providers.lm-studio]
kind = "openai-compatible"
base_url = "http://127.0.0.1:1234/v1"
api_key = "lm-studio"
model = "qwen-2.5-7b"
```

```toml
# .codewhale/agents/local-formatter.toml
id = "local-formatter"
role_hint = "formatter"
provider = "lm-studio"
model = "qwen-2.5-7b"
reasoning_effort = "off"

[instructions]
text = "Use small, local edits. Keep formatting changes mechanical."
```

然后调用 `agent(profile: "local-formatter", prompt: "...")`。进程内的子智能体会为 `lm-studio` 构建一个客户端；Fleet `worker` 会把 `--provider lm-studio` 转发给 `codewhale exec`，由它解析同一个 `[providers.lm-studio]` 表。未知或未配置的提供商 id 会让生成失败，而不是悄悄回退到父智能体的提供商。

## 单步 API 超时（#1806、#1808）

每个子智能体步骤都把它的 DeepSeek `create_message` 调用包在一个单步超时里，这样单个卡住的请求就不会无限期地占住父智能体的完成唤醒通道。默认值是 `600` 秒。超时的尝试会以指数退避重试（最多 5 次），之后该步骤带着保留的检查点中断。合理地需要超过这个时长的长思考子智能体，例如 `agent` 背后繁重的 plan 或 review 工作，可以在 `~/.codewhale/config.toml` 中延长超时：

```toml
[subagents]
api_timeout_secs = 900  # 15 minutes; clamped to 1..=3600
```

取值会被限制在 `1..=3600`。`0` 和 `unset` 都保持 `600` 秒的默认值。

## 陈旧智能体心跳（#2614）

运行中的智能体还会跟踪管理器可见的进度。如果某个子智能体在心跳窗口内停止发出进度，管理器会自动取消它，释放它的子智能体槽位，并通过返回的转录句柄和持久化的 `worker` 记录保留可检查的已取消记录。默认值是 5 分钟（解析为至少比 `api_timeout_secs` 高 30 秒，因此在 600 秒的默认 API 超时下是 630 秒）：

```toml
[subagents]
heartbeat_timeout_secs = 300  # clamped to 30..=3600
```

有效的心跳会保持至少比 `api_timeout_secs` 高 30 秒，这样一个配置得很长的模型请求，不会在它自己的请求超时触发之前就被取消。

## 生命周期

每个打开的会话都会产生一条记录，按如下顺序推进：

```
Pending → Running → (Completed | Failed(reason) | Cancelled | Interrupted(reason) | BudgetExhausted)
```

显式中断、提供商重试耗尽，或恢复一条孤立的运行中记录，都可能留下一个带检查点的 `Interrupted` `worker`。请查看 `needs_continuation` 和记录的原因；对可继续的工作使用 `followup`。`BudgetExhausted` 包含具体的 token、步数或墙钟时间原因；续跑无法补足已耗尽的额度。

`wait` 只观察 `worker`。超时返回当前的结果，绝不会停驻、取消或恢复它们。`until: "completion"` 在有一个子智能体结算时返回；`until: "all"` 汇合该调用开始时正在运行的那批 `worker`；`until: "activity"` 可以在有进展时返回。之后的生成不会被悄悄加进更早的汇合中。

父智能体的一次普通响应会让健康的子智能体继续运行。同一个 Engine 回合循环会消费它们的完成通知，并可以继续父智能体。无界面的 `codewhale exec` 会推迟成功的最终回执，直到它现有的 Engine 报告没有存活的子智能体、也没有排队的子智能体完成通知为止。它原本的墙钟截止时间仍然约束这一结算过程，包括自主的父回合。取消、截止时间耗尽、致命事件或丢失 Engine 通道，都会停止结算，并返回相应的 interrupted 或 failed 回执，附带已记录的部分用量。它不会仅仅因为父智能体的第一次响应结束了，就报告子智能体成功完成。

### 会话边界（#405）

每个 `SubAgentManager` 实例在构造时都会给自己分配一个全新的 `session_boot_id`。每个新会话都用该 id 给智能体盖章；工作区状态文件记录它，用于重启恢复。

任务面板（workbar）/状态投影默认聚焦当前会话的智能体。不再运行的先前会话的智能体被视为归档记录，这样模型就不会把陈旧的工作误认为活跃的工作。这只是一条*先前会话*规则：在当前会话中完成的智能体，会在会话剩余时间内保留它们在任务面板中的行（安静完成），它们的详情仍然可以从那些行打开。

从 #405 之前的持久化状态文件加载的记录（没有 `session_boot_id` 字段）会被归为先前会话，因为管理器无法把它们与当前这次启动对上号。

## 运行回执、后续消息与接管

每个兼容的子智能体在 `.codewhale/state/subagents.v1.json` 中都有一条持久化的 `worker` 记录。在这些通道直接由 Fleet 账本支撑之前，这条记录是子智能体通道当前的运行账本切片：它存储 `run_id`、目标、角色/模型、workspace/分支、生命周期事件、工件引用、后续目标、接管目标、用量来源和验证来源。

正常的父智能体流程是继续工作并消费完成事件。默认的启动和状态回执是紧凑的；完整快照和 `worker` 记录属于诊断细节，不会在每个响应里重复出现。

### 继续一个已有的 worker

`message` 只把一条备注排入队列，不唤醒子智能体。`followup` 会唤醒一个运行中的子智能体，或从一个可继续的检查点恢复：

```json
{"action":"followup","agent_id":"child-previous-id","message":"Continue the assignment using the recorded evidence."}
```

之后的等待和消息请使用返回的 `agent_id`。回执中的 `from` 和 `to` 标识原始目标及其当前的延续。原始回执会被保留。通过旧 ID 重试会沿着持久化的延续链走，不会创建重复的 `worker`。如果当前的后继者正在运行，后续消息就送达到那里；如果它已经结算且无法继续，响应会说明没有消息被送达。重复的 `worker` 会被避免，但对一个运行中 `worker` 的重复消息，仍然是重复的消息。

对于批量，请恰好选择一种目标形式：

```json
{"action":"followup","agent_ids":["child-a","child-b"],"message":"Continue the remaining checks."}
```

```json
{"action":"followup","all_parked":true,"message":"Continue the parked assignments."}
```

显式的批量最多接受 32 个互不相同的 ID。`all_parked` 选择你所控制的已停驻子智能体，并在超过 32 个时拒绝，以便你改选显式批量。批量响应会分别返回 `results` 和 `errors`；某个目标失败，不会回滚已成功的续跑。父/后代的控制检查同时适用于被寻址的记录和它当前的后继者。

只有在要从一个已结算子智能体的转录创建一个单独的 `worker` 时（例如分派一项新的审查），才使用带 `resume_from` 的 `start`。每一次这样的 `start` 都是一个新的 `worker`。缺失的、正在运行的或跨工作区的来源会被拒绝；来源的权限和预算界限仍然适用。这与用 `followup` 继续已停驻的工作是不同的。

### 紧凑状态与完整转录获取

未限定范围的 `agent(action="status")` 返回一页以会话为范围、上限为 8 KiB 的结果。`offset` 和 `limit` 对名册分页；默认值和最大值都是 20。请沿着 `next_offset` 继续，因为字节上限可能使返回的行数少于请求的数量。面向模型的名册传输格式使用一个稳定的 `columns` 表头，`agents` 中每个条目是一个值数组；请把每一行与表头配对，而不要把它当作对象来读。`null` 表示缺失或未报告，测得的零则保持为数值 `0`。即使是空页，表头也会出现。这些行包含 `worker` 和父智能体的 ID、当前深度、状态、耗时、自身的 token 总量、最近的活动、待处理的输入，以及续接谱系（`resumed_from` / `resumed_as`）。验证信息包含判定、非空交付物的计数，以及必要时的一条简短警告。名称、步数、路由、有效限制（包括最大深度）和 token 明细，在寻址某一个 `agent_id` 时，仍然通过未改变的对象投影提供。汇总用量把每个 `worker` 自己报告的 token 只计一次，并报告其覆盖范围。完成回执还会报告测得的后代用量，对续接谱系去重，并把未知用量与零区分开。`worker` 的 `has_unreported_usage`，以及后代/子树的 `unreported_usage_workers` 计数，即使后来的响应提供了已测得的小计，也能标识出缺失的响应。

调查失败时，可以请求某一个 `worker` 的详情：

```json
{"action":"status","agent_id":"child-a","detail":true,"offset":0,"limit":20}
```

被寻址的 `peek` 同样接受 `detail: true`。详情仍以 32 KiB 为界；消息/事件归档和交付物判定是分页的，省略字段会标出被截断的详情。使用返回的类型化 `transcript_handle` 配合 `handle_read`，读取完整保留的转录。即使诊断性文字被省略，句柄的查找坐标也会被保留。未限定范围的 `detail: true` 不会把整个名册展开成转录。

工件是符号引用。请把 `result_summary` 视为子智能体的自我报告，并在依赖它之前，先检查具体的 `verification.status` 及其证据。`usage.status` 在提供商报告用量之前保持 `unknown`，之后对已耗尽的 token 作用域变为 `reported` 或 `budget_exhausted`。无论是文件的 `present` 判定，还是已完成的生命周期状态，都不能证明测试关卡已通过。

## 输出契约

非 scout 的子智能体以五个 Markdown 标题结尾，顺序如下：

```
### SUMMARY    one paragraph; what you did and what happened
### EVIDENCE   path:line-range citations and key findings; one bullet each
### CHANGES    files modified, with one-line descriptions; "None." if read-only
### RISKS      what could go wrong / what the parent should double-check
### BLOCKERS   what stopped you; "None." if you finished cleanly
```

（各标题后面的说明文字是发给模型的协议文本，保持英文原样；含义依次是：一段话，说明做了什么、发生了什么；`path:line-range` 形式的引用和关键发现，每条一个要点；修改过的文件及一行描述，只读则写 "None."；可能出什么问题 / 父智能体应该复核什么；什么阻止了你，干净完成则写 "None."。）

使用 `### HEADING` 形式的行，且 `EVIDENCE` 要在 `CHANGES` 之前。把编辑过的、相对仓库的文件路径列在 `### CHANGES` 之下；要点之前允许有空行。每个要点以文件路径开头，后面跟描述；含空格或字面末尾标点的路径需要加引号。校验器也接受较旧的显式声明 `CHANGES:`、`Changed files:` 和 `Files changed:`。`EVIDENCE` 中的证据引用以及 `RISKS` 之下的路径，都不算编辑声明。这份五标题的提示词契约是 `crates/tui/src/prompts/text.rs` 中的 `SUBAGENT_OUTPUT_FORMAT`。`crates/tui/src/prompts.rs` 中的 `prompt_documents_structured_subagent_briefs` 会对照它断言每一个标题。

Scout 是例外（#5189 F5）：它们只以 `### SUMMARY` 和 `### EVIDENCE` 结尾（`crates/tui/src/prompts/text.rs` 中的 `SUBAGENT_SCOUT_OUTPUT_FORMAT`）。`crates/tui/src/tools/subagent/mod.rs` 中的 `FleetRole::system_prompt` 为 `FleetRole::Scout` 注入 scout 契约，为其他每个角色注入五标题契约。一项子智能体测试钉住了：scout 包含 `## Output contract (scout)`，且不包含 `### BLOCKERS`。

父智能体把 `EVIDENCE` 当作下一回合的工作集来读，所以 scout 和 reviewer 在这里应当精确。

## 记忆与 `remember` 工具（#489）

启用记忆时（`[memory] enabled = true` 或 `DEEPSEEK_MEMORY=on`），子智能体共享父智能体的原生记忆存储。它们可以通过 `remember` 工具追加持久的笔记——这对一个发现了值得跨会话保留的项目约定的 scout，或一个得知“这个测试不稳定”的 verifier 很方便。

`remember` 接受 `global` 或 `workspace` 作为 `scope`（`crates/tui/src/tools/remember.rs`，schema 枚举加上 scope 处理），并通过 `NativeMemoryStore` 写入 `~/.codewhale/memory/global/MEMORY.md` 或 `~/.codewhale/memory/workspace/<id>/MEMORY.md`。写入不经过标准的写入审批流程。旧版的单文件 `memory.md` 路径已在 v0.9.4 中移除；完整布局参见 `docs/MEMORY.md`。

## 实现说明

- 源码：`crates/tui/src/tools/subagent/mod.rs`。
- 持久化状态：`<workspace>/.codewhale/state/subagents.v1.json`。schema 版本 `1`（向前兼容——新的可选字段使用 `#[serde(default)]`）。
- 已结算的记录通常在 `COMPLETED_AGENT_RETENTION`（默认 1 小时）之后过期，正常保留记录的目标数量为 256。运行中/启动中/等待中的 `worker`，以及存活工作所需的延续身份和预算谱系都会被保留。清理不能在一个旧 ID 的延续仍然活跃时丢弃它，也不能抹掉为强制执行某个活跃作用域所需的用量历史。
- `SubAgentRuntime::background_runtime()` 从 `child_runtime()` 出发，但用一个全新的取消令牌替换了回合范围的子令牌，因此父回合的取消不会停止 detached 的后台会话。
- `is_running` 检查会忽略 `task_handle` 为 `None` 的智能体；这样可以避免把已持久化但已脱离的记录计入并发上限（#509）。
- `SharedSubAgentManager` 是 `Arc<RwLock<...>>`——读路径使用读锁，因此 `/agents` 和任务面板（workbar）投影在多智能体扇出期间不会阻塞主循环（#510）。

个人 profile 使用同样的格式，位于 `$CODEWHALE_HOME/agents/<id>.toml`（通常是 `~/.codewhale/agents/`）。例如：

```toml
# ~/.codewhale/agents/reasoner.toml
base_role = "explore"
provider = "openrouter"
model = "qwen/qwen3.7-plus"
reasoning_effort = "high"

[permissions]
allow_shell = false
trust = false
```

用 `agent(action: "start", profile: "reasoner", prompt: "...")` 选择它。该提供商还必须在 `config.toml` 中配置好。回执会写明解析后的 profile、它的个人/项目来源、提供商/模型和有效的思考强度。强度会被归一化到所选模型支持的层级；显式的 `thinking` 请求会覆盖保存的偏好。

`allow_shell` 和 `trust` 属于 `[permissions]`，而不是顶层。profile 不能授予 `allow_shell = true`、`trust = true`，也不能关闭审批。请为任务选择合适的 `base_role`；父会话的实时策略仍是权限的天花板。这些 profile 字段并不是授予额外访问权的途径。

格式错误、不可读或重复的 profile，现在会导致明确的选择错误，包括其名称与内置角色同名的情形。它绝不会悄悄地用更低一层的名册来替代。请修复该文件后重试；profile 在每次启动时都会重新加载。`agent(action: "roster")` 会在 `profile_load_issues` 中报告受影响的 profile 身份和路径，而不暴露解析器的摘录。其他有效的 profile 仍然可用，有效的项目覆盖仍然优先于损坏的个人定义。Fleet 运行创建在存储运行或启动 `worker` 之前，会执行同样的检查。
