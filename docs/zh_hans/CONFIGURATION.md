# 配置（Configuration）

> 英文原文：[CONFIGURATION.md](../CONFIGURATION.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。
> 2026-10-06 补齐流传输、脚本工具及用户等待契约；其余段落保留原同步日期。
> ChatGPT 登录相关内容于 2026-10-01 按当前实现更新。

Codewhale 从 TOML 文件加环境变量读取配置。进程启动时，它还可能从工作区本地的 `.env` 文件加载字面(literal)的内置 provider 凭据。请以受跟踪的 `.env.example` 为模板；把它复制为 `.env`，然后只添加凭据值。

工作区不具备配置权威性。因此 Codewhale 会忽略 config/profile/home 路径、provider/模型/base-URL 路由、MCP/插件状态、审批/沙箱/shell 档位、可执行文件路径、运行时设置，以及 `.env` 里每一个其他非凭据条目。变量展开被拒绝，所以仓库不能把环境中的机密替换进凭据值。请用 `config.toml`、CLI 标志或启动 shell 导出的值来设置这些显式的控制面设置。`.env` 通过稳定的常规文件句柄读取，上限 1 MiB，符号链接、重解析点和多链接文件都会被拒绝。

## 从 CLI 读取和检查配置

`codewhale config get <key>` 可以读取标量键、`tools` 这样的整张表，以及 `tools.user_input_timeout_seconds` 这样的嵌套路径。显示出的表和嵌套值与 `config dump` 一样，应用相同的递归凭据脱敏。

`config set` 支持它已命名的标量键，以及 provider、route 和通知命令。它会拒绝没有任何代码读取的键，并给出最接近的真实键作为建议（`config set calm_mod on` 会指出 `calm_mode`）。像 `calm_mode` 或 `tool_collapse` 这样的 settings.toml 键会被校验并写入用户全局的 settings.toml，而不是 config.toml；`config get` 会从那里读回它，即使仍留有一份旧的 config.toml 副本（没有任何代码读取它），并指出那份副本，方便你用 `config unset` 清掉。settings 键没有项目作用域，所以 `--project` 会拒绝它们。config.toml 的键会按其读取方所期望的类型检查，并在读取方需要时存为 TOML 布尔值或数字（`yolo = true`、`max_subagents = 4`）；`reasoning_effort` 接受与 `/effort` 相同的别名（#6563）。其他带点号的写入会在修改文件之前失败，并指出应当编辑的 TOML 表。例如，要在文件中设置工具超时，写成：

```toml
[tools]
user_input_timeout_seconds = 0
```

`codewhale config doctor` 会检查凭据是否存在和端点形态，并对 config.toml 中每个没有任何代码读取的根键给出警告：留在 config.toml 里的 settings.toml 键会被指出放错了位置，任何其他未被读取的键会得到一个“你是不是想输入……”的建议。任何运行时读取方读取的键都不会被报告。这些警告不会使检查失败，而干净的结果也不代表已校验每个运行时设置的取值（#6083、#6563）。

## 宪章、项目指令与仓库权威

Codewhale 有多个指令层级（instruction surfaces）。它们刻意保持分离，这样个人宪章、仓库策略、项目指令和运行时安全控制就不会被混淆。

- **内置全局宪章(Bundled global Constitution)**——编译进二进制的基础规则。它是每个会话的默认底线。
- **用户全局宪章(User-global constitution)**——常规引导式设置的产物。用 `/constitution` 或 `/setup` 管理；Codewhale 把结构化数据存放在 `$CODEWHALE_HOME/constitution.json`(默认 `~/.codewhale/constitution.json`)，并渲染成独立的 `<codewhale_user_constitution>` 散文块（prose block）。它可以表达偏好和停止条件，但不会改变运行时审批策略、沙箱、shell、网络、信任或 MCP 权限。
- **仓库本地宪章(Repo-local constitution)**——可选的 `.codewhale/constitution.json` 项目策略，见下文。
- **`AGENTS.md`**——跨智能体**项目指令**(散文)。这是"智能体应如何在这个仓库工作"的规范文件。运行 `/init` 生成一份。为其他工具编写的指令文件(`CLAUDE.md`、`.claude/instructions.md`、`.cursorrules`、`.github/copilot-instructions.md` 等)默认不读取；通过顶层配置项 `project_instruction_imports` 选择导入，例如 `["claude"]`、`["cursor"]` 或 `["all"]`(见 `config.example.toml`)；发现未加载的此类文件时，Codewhale 会给出提示。**可选(`#6526`)**：设置 `claude_fallback = true` 后，某目录没有 `AGENTS.md`(也没有 Codewhale 自己的 `instructions.md`)时会回退读取该目录的 `CLAUDE.md`；默认关闭。

> **指令预算 `[instruction_budget]`(#6526)**：`global` 与 `project` 两个作用域分别用**估算 token** 设上限，取「绝对 token 上限」与「可用窗口百分比」中较小者(默认各 100,000 token、25%)；可用窗口 = 上下文窗口 − 本次请求实际预留的输出 − 10% 安全余量。每个文件最多保留前 100 KiB，超过 `file_max_bytes`(默认且最大 8 MiB)的文件会被跳过。每一次裁剪、跳过或拒绝都会作为运行时消息告知模型，并出现在 `/context report` 中；`policy = "refuse"` 会在请求发出前停止。该表只能写在用户配置中，仓库内的项目配置会被忽略。完整说明和示例见英文版 `docs/CONFIGURATION.md` 与 `config.example.toml`。
- **记忆与交接(Memory and handoffs)**——被召回的状态。有用，但权威低于宪章和项目指令。

### 管理用户全局宪章(`/setup` 和 `/constitution`)

内置的**协作约定(working agreement)**是安全默认，不再需要强制的首次启动屏幕。之后通过 `/constitution` 或渐进式 `/setup` 指南自定义它。Provider/模型就绪状态、工作区信任和运行时权限档位与此指引保持分离。

在**宪章(Constitution)**步骤：

- **`1`–`6`** 调整引导式草稿。**`G`** 预览它，再次按 **`G`** 批准并保存一份新的结构化 `constitution.json`。
- **`A`**(仅在已配置 provider 时显示)让你配置的第一个模型起草宪章。起草**不是**保存：草稿会通过同样的预览渲染，你仍然要按 **`G`** 批准后才会持久化任何内容。
- **`K`** 保持你现有的已加载宪章不变(仅在已有有效文件时显示)。
- **`U`**(或 `/constitution bundled`)记录内置/默认宪章。

`/constitution`(别名 `/law`)是设置完成后主要的管理面。子命令：`status`(默认)、`preview`、`review`、`repo`(仓库本地宪章)、`explain`、`edit`/`guided`、`repair`、`posture` 和 `bundled`。管理宪章永远不会改变运行时审批、沙箱、shell、网络、信任、默认模式或 MCP 权威——这些都留在运行时权限档位/配置里。

每个仓库可以携带两个不同且互补的文件：

- **`AGENTS.md`**——普通的项目工作指令。
- **`.codewhale/constitution.json`**——Codewhale 特有的**仓库权威/优先级策略**：当本地来源冲突时，Codewhale 应该先信任谁，以及在声称任务完成之前要验证什么。`.codewhale/` 位于仓库内部(像 `.github/` 一样)。例如：

  ```json
  {
    "schema_version": 1,
    "authority": [
      "current user request",
      "live code and tests",
      "GitHub issue/PR details",
      "AGENTS.md",
      "memory",
      "old handoffs"
    ],
    "protected_invariants": [
      "do not break old-session transcript replay"
    ],
    "branch_policy": "PRs target the integration branch, not main",
    "verification_policy": {
      "before_claiming_done": ["run focused tests", "read changed files back"]
    },
    "escalate_when": [
      "a destructive action was not explicitly authorized"
    ]
  }
  ```

  所有字段都是可选的。存在时，该文件会被渲染进系统提示，作为更高权威块中的简洁散文。旧的 `WHALE.md` 文件会被忽略，并报告为仅迁移诊断。

  每个 `protected_invariants` 条目可以是普通字符串(建议性散文，历史形态)，也可以是携带路径 glob 的对象，后者会在工具门禁中额外被**机械强制执行**。见下文[强制执行的仓库保护规则](#强制执行的仓库保护规则)。

  这是**仓库本地宪章**层。指引发生冲突时，以当前生效的基础宪章中 **Whose word wins** 一节为准；内置版本的源代码位于 [`BASE_PROMPT`](../../crates/engine/src/prompts/text.rs)。使用 `/constitution base` 可查看实际生效的基础提示词，包括明确启用的专家覆盖版本。这里介绍文件的顺序、以及提示词的组装顺序，都不代表权威排序。运行时策略（代码强制执行的权限、沙箱和成本上限）独立于提示词指引；编辑宪章不会授予权限。

> **`WHALE.md` 已弃用。** 它与 `AGENTS.md` 混淆重叠。Codewhale 不再把 `WHALE.md` 作为项目或全局上下文读取。如果存在，setup/上下文诊断会报告它被忽略，以便你迁移它。把普通指令移到 `AGENTS.md`，把 Codewhale 特有的权威策略移到 `.codewhale/constitution.json`。个人常驻指引属于 `/constitution` / `$CODEWHALE_HOME/constitution.json`。(随模型提示一起提供的全局 Codewhale 宪章是另一回事，不受影响。)

### 强制执行的仓库保护规则

默认情况下，`protected_invariants` 条目是建议性散文：它被渲染进提示，作为智能体应遵守的指引，但没有任何东西会阻止写入。写成**带 `paths` 的对象**的条目则不同——它会编译成机械写入拦阻(hold)，由引擎的工具门禁在写入运行之前评估。这些规则由代码强制执行，不再只是提示中的要求。

强制条目具有这样的形态：

```json
{
  "schema_version": 1,
  "protected_invariants": [
    "Keep DeepSeek support first-class.",
    {
      "text": "The wire format is frozen; protocol changes need a human.",
      "paths": ["crates/protocol/**"],
      "action": "block"
    },
    {
      "text": "Release notes need human review.",
      "paths": ["CHANGELOG.md"],
      "action": "ask"
    }
  ]
}
```

- `text`——必填。拦阻时展示的理由。空的 `text` 会被跳过。
- `paths`——工作区相对 glob(globset 语法，例如 `crates/protocol/**`、`**/secrets.toml`、`CHANGELOG.md`)。没有可用 `paths` 的对象即使形态是对象也仍只是建议性的。
- `action`——可选，默认 `ask`。`ask` 在 Ask 和 Auto-Review 中强制弹窗；在 Full Access 中则拒绝受保护的写入而不打开模态框。`block` 在每个权限档位中都**直接拒绝写入**。

语义：

- **只收紧(Tighten-only)。** schema 没有 allow/widen 形态，所以规则只能*增加*拦阻——精心构造的宪章永远不能授予权威或削弱其上的门禁。
- **模式不能绕过。** 与内置安全底线一样，`ask` 拦阻在 Ask 和 Auto-Review 中强制弹窗。Full Access 从不打开审批模态框，所以同样的拦阻按失败即阻断(fail closed)处理为硬阻断；`block` 总是拒绝。模式无法关掉拦阻。
- **仅仓库本地。** 只有仓库的 `.codewhale/constitution.json` 参与。用户全局宪章保持建议性散文，永远不进入这个机制。
- **失败安全。** 文件缺失、解析错误或无效 glob 会退化为更少或零规则——绝不会在未受保护的路径上产生拦阻，也绝不会让门禁中毒。跨匹配时最强的动作胜出，所以 `block` 高于 `ask`。
- **留下回执(receipt)。** 每次拦阻都会发出 `tool.repo_law_decision` 工具审计事件，指名不变项、匹配的路径和源文件；批准/拒绝理由也会指名不变项。

**覆盖范围刻意有限。** 拦阻只对写入工具 `write_file`、`edit_file`、`apply_patch` 和 `fim_edit` 评估，并且只针对它们输入中指定的文件系统目标(`path`/`target`/`destination`/`file_path`、`changes[].path`，以及 unified-diff / `apply_patch`-envelope 头)。一条写入受保护路径的 shell 命令**不会**被仓库保护规则拦阻——这类写入仍由普通审批、沙箱和 shell 写入门禁管辖，不由这个机制管辖。

### 专家级完整基础提示覆盖(#3638)

全局宪章(基础系统提示，通常从 `crates/engine/src/prompts/text.rs` 编译为 `BASE_PROMPT`)可以不重新构建就按用户替换。这是专家逃生舱，不是常规 `/constitution` 引导式设置的产物。因为这是一个提示信任边界，它需要**两个刻意的步骤**——单靠文件不够：

1. 把替换文件放到 `~/.codewhale/prompts/constitution.md`(设置了 `$CODEWHALE_HOME` 时在其下)。
2. 设置显式的选择加入标志 `CODEWHALE_ALLOW_BASE_PROMPT_OVERRIDE=1`(也接受 `true`/`on`/`yes`)。

如果文件存在但标志未设置，覆盖会被**忽略**(有一条指向该标志的日志行)，内置宪章保持不变。这用于把 TUI 重新用于软件工程之外的场景——例如长文写作或文档审查——此时面向工程的基座提示不合适。它在启动时加载一次；**缺失或空文件是空操作**，所以现有安装会保留内置提示。

范围刻意很窄：只有字节稳定的**基础提示段**可被覆盖。模式增量、审批策略、工具分类、上下文管理和压缩中继仍由 Codewhale 的运行时组装拥有，所以覆盖**无法移除安全相关指引**(沙箱、审批)——它只替换任务/语气框架。要定制普通的个人行为，优先用 `/constitution`；要定制按仓库行为，优先用上面的 `AGENTS.md` + `.codewhale/constitution.json`。

## 配置在哪里读取(Where It Looks)

默认配置路径：

- `~/.codewhale/config.toml`
- 旧版回退：`~/.deepseek/config.toml`

覆盖：

- CLI：`codewhale --config /path/to/config.toml`
- 环境：`CODEWHALE_CONFIG_PATH=/path/to/config.toml`
- 旧环境别名：`DEEPSEEK_CONFIG_PATH=/path/to/config.toml`

如果两者都设置，`--config` 胜出。环境变量覆盖在文件加载后应用。

### TUI 可编辑性审计

在 TUI 内运行 `/config audit`，查看哪些已记录的键可以从当前会话更改、哪些还可以持久化、哪些只能文件级或重启级。审计包含高影响运行时控制的当前值，例如 `approval_policy`、`allow_shell`、`stream_chunk_timeout_secs`、`base_url`、`mcp_config_path` 以及 `[subagents]` 的并发/深度/超时键。

手工编辑前，以该命令的"Command / reason"列为事实来源。例如，`/config approval_mode on-request --save` 写入顶层 `approval_policy = "on-request"`，而 provider base URL 可以保存但仍需要重启模型客户端。

### 用户工作区条目

交互式 Agent 会话默认在审批门禁下暴露 shell 工具，除非你显式禁用它们。对于应放在用户全局配置里、供非交互或持久任务 profile 使用的 shell 选择加入，而不是放在仓库里，请添加工作区作用域条目：

```toml
[workspace.'/absolute/path/to/project']
allow_shell = true
```

该条目仅在启动的工作区路径匹配表键时生效。旧版 `[projects."/absolute/path/to/project"]` 表也接受用于这种用户自有的覆盖。

在交互模式下，按项目覆盖 `<workspace>/.codewhale/config.toml` 会在这个用户条目之后应用。项目级的 `allow_shell = false` 仍可收紧会话；项目级的 `allow_shell = true` 会被忽略。

### 按项目覆盖(#485)

当 TUI 在一个包含常规文件 `<workspace>/.codewhale/config.toml` 的工作区启动时，该文件中声明的安全值会合并到全局配置之上。旧的 `<workspace>/.deepseek/config.toml` 文件在 Codewhale 路径不存在时仍会被读取。符号链接的项目配置文件会被拒绝。这让仓库可以建议模型或收紧本地安全档位，而不触碰用户的 `~/.codewhale/config.toml`。传 `--no-project-config` 可跳过单次启动的覆盖。

项目覆盖中支持的键(仅顶层字段):

| 键 | 效果 |
|---|---|
| `model` | 覆盖 `default_text_model` |
| `reasoning_effort` | 为复杂仓库强制 `"high"` / `"max"` |
| `approval_policy` | 只接受收紧用户当前权限档位的值 |
| `sandbox_mode` | 只接受收紧用户当前沙箱档位的值 |
| `max_subagents` | 为受限仓库限制子智能体并发(钳制到 `1..=128`) |
| `allow_shell` | `false` 可禁用 shell 访问；`true` 被忽略 |

覆盖刻意很窄——它覆盖仓库维护者最可能想要跨贡献者标准化的字段。凭据、端点、provider 选择、MCP 配置、hooks、skills、重试、热栏绑定和 `instructions = [...]` 设置保持用户全局。如果仓库本地配置声明了 `api_key`、`base_url`、`providers`、`provider`、`mcp_config_path`、`notes_path`、`hotbar`、`allow_shell = true` 或 `instructions`，Codewhale 会忽略该键并保留用户的全局设置。

合并的 `codewhale` 运行时使用 `~/.codewhale/config.toml` 保存提供商和模型设置。
`codewhale auth set --provider deepseek` 把密钥保存在本地机密存储中（默认是
`~/.codewhale/secrets/` 下的私有文件；OS 钥匙串需显式选择），而不是写入该配置文件。
需要时，首次启动会迁移旧 `~/.deepseek/config.toml`。
`codewhale --model deepseek-v4-flash` 作为 `CODEWHALE_MODEL` 转发给 TUI。
分发器不再写入对应的 `DEEPSEEK_*` 变量；用户自己设置的值在 `CODEWHALE_*`
未设置时仍作为旧别名读取。

### 账号提供商密钥

新的 Codewhale 账号暂未开放，本指南也不建议把提供商密钥保存在账号中。请用 `/provider`
或 `codewhale auth set --provider <provider>` 把密钥保存在你自己的机器上；使用本地密钥或
本地模型从不需要账号。

如果你已经有账号，以下命令用于管理保存在账号中的密钥：

```bash
codewhale account keys list              # configured status, never secret values
codewhale account keys remove deepseek   # remove the account's saved key
```

登录绝不会上传本地密钥；移除账号中的密钥不会改变本地副本，也不会撤销提供商侧的密钥。

账号会话与提供商密钥分开保存。`codewhale account status` 查看会话；
`codewhale account logout` 移除所选 `--profile` 的账号会话，不删除提供商密钥。
账号会话使用私有 `0600` Codewhale secrets 文件，按 profile 和账号 API 来源隔离，
不使用 OS 钥匙串。旧的 `codewhale cloud ...` 仍是账号命令的别名。

### 可移植配置包（Portable config bundles）

`codewhale config export --portable [--project] [--out FILE]` 写出一份可移植、不含机密的配置包：排序后的 TOML，其中凭据和机器特有的键（API key、base URL、socket 路径）会被丢弃，绝不会用脱敏占位符顶替。不带 `--out` 时，配置包输出到 stdout。类型化的表、数组、数字、布尔值和日期时间保持其类型。与机器绑定的权限刻意设为不可移植：项目信任覆盖、凭据读取器、会自动运行的 hooks、可执行的 LSP 定义和本地路径绑定都会被省略，而不是复制到新主机。信任相关设置（`yolo`、`allow_shell`、`approval_policy`、`sandbox_mode`、`sandbox_network_access` 以及沙箱读取拒绝列表路径）以及本地端点或可执行文件（`[lifecycle_outbox]`、`[extension_host]`、`[control_socket]`）同样如此。带有凭据的 URL——含 userinfo、含名称表明它携带令牌的查询参数，或 Slack/Discord/Teams webhook 路径——按凭据处理。

`codewhale config import <FILE|HTTPS_URL|-> [--dry-run] [--yes] [--project]` 应用一份配置包。信封是严格的（`schema_version = 1`，kind 为 `codewhale.portable-config`；未知字段会失败）。导入会打印一份确定性的计划——新增 / 更改 / 跳过 / 冲突 / 拒绝——然后征求同意，除非给了 `--yes`；无头使用则必须带它。形似凭据的条目会按键名和值的形态被拒绝；拒绝信息会指出字段，绝不会指出值。远程配置包只允许来自 HTTPS（loopback 的 http 除外），上限 5 MiB。应用时会把目标文档备份到 `<config>.bundle-backup-<timestamp>-<random>`，任何失败都会回滚，并且重新导入已应用过的配置包不会改变任何东西。

导入还会拒绝导出所省略的那些不可移植的权限类别，包括嵌套/驼峰/点号形式的凭据键和 cookie 头。这样，手工编写的或远程的配置包就无法重新引入机器信任、本地凭据访问，或本地导出会拒绝携带的可自动执行命令。结构化的表会被深度合并：可移植的模型或偏好更新不会抹掉目标本地的 provider 凭据、端点、hooks 或可执行定义——这些是刻意从配置包中省略的。数组和标量值仍会替换对应的可移植值。

各节对应作用域：`[project]` 条目只会落入工作区文档（`--project`，必须指向真实的工作区配置），`[global]` 只会落入用户全局文档；`preferences`、`profiles` 和 `plugins` 在任一作用域都适用。全局配置包操作会拒绝工作区文档，正如项目操作会拒绝用户全局文档一样。

### 凭据读取优先级(#5197)

凭据读取**默认与文件夹无关**：下面的每一层都是用户全局或进程作用域，所以在一个仓库保存的 key 会在每个其他仓库以相同方式解析。仓库本地配置从不携带凭据材料——上面的项目覆盖只读取其 allowlist 键并忽略 `api_key`，而针对工作区作用域配置的凭据写入会被重新定位到用户全局 `~/.codewhale/config.toml`(#5045、#5193)。

对于活动 provider，运行时按以下精确顺序解析 API key(首个匹配胜出):

1. **路由专用认证。** `auth_mode` 禁用 API 密钥的路由在此停止，不携带凭据。官方 `openai-codex` 套餐路由只接受 `codewhale auth chatgpt` 获取的、由 Codewhale 自己保管的授权；签发的注册信息、已验证的账户和套餐权限必须匹配。Codex CLI 导入令牌、旧版进程令牌、API 密钥和机密存储槽位都不能替代这项授权。`[providers.xai] auth_mode = "oauth"` 使用 Codewhale 自有的 xAI 设备登录存储，或经精确路径只读授权的 Grok CLI 文件。
2. **显式 CLI key。** `--api-key` 连同其来源标记转发，胜过每一个已保存的槽位。
3. **配置文件 `api_key`。** 活动 provider 的 `[providers.<name>] api_key` 表槽位。`deepseek-CN` 也读取 `[providers.deepseek]`。较早的顶层 `api_key` 会被当作 `[providers.deepseek] api_key` 来读取（见[旧版顶层 `base_url` 与 `api_key`](#旧版顶层-base_url-与-api_key)）。文件拥有的 key 保持绑定到它们文件拥有的端点：当环境用自定义主机替换路由的 base URL 时，已保存的 key 不会被发送到那里。
4. **`api_key_env` 绑定。** `[providers.<name>] api_key_env = "VAR"` 命名环境变量。对于自定义 provider，未设置或空绑定是响亮错误，而不是静默回退(#5104)。
5. **Secret store。** 由 `codewhale auth set` 写入的持久按 provider 槽位(默认文件型位于 `~/.codewhale/secrets/` 下；只有显式选择时才用 OS 钥匙串)。对命名的自定义路由、自托管 provider、除显式认证的 loopback 之外的自定义端点，以及其 `auth_mode` 不需要 key 的路由，跳过。
6. **周围环境(Ambient environment)。** provider 自己的变量(`DEEPSEEK_API_KEY`、`OPENROUTER_API_KEY`、`MOONSHOT_API_KEY`……)。环境 key 只会被发送到 provider 的官方端点，并且在与 secret store 相同的条件下被跳过。
7. **无 key 回退。** 自托管 provider 和 loopback 端点可以在没有凭据的情况下运行；其他每条路由都会以 provider 特有的设置指引失败。

旧版兼容：`~/.deepseek/config.toml` 在首次启动时迁移进 `~/.codewhale/config.toml`，`DEEPSEEK_*` 环境变量仍作为 `CODEWHALE_*` 形式的别名被接受，`DEEPSEEK_SECRET_BACKEND` 是 `CODEWHALE_SECRET_BACKEND` 的旧别名。

运行 `codewhale auth status` 检查活动 provider 的配置文件、OS 钥匙串后端、环境变量、胜出来源和末四位标签，而不打印 key 本身。该命令只探测活动 provider 的钥匙串条目。

对于托管、通用 OpenAI 兼容、自托管、OpenAI Responses 或原生 Anthropic provider，设置 `provider = "<id>"` 或传 `codewhale --provider <id>`。规范的 provider ID 是 `deepseek`、`nvidia-nim`、`openai`、`atlascloud`、`wanjie-ark`、`volcengine`、`openrouter`、`orcarouter`、`xiaomi-mimo`、`novita`、`fireworks`、`siliconflow`、`arcee`、`siliconflow-CN`、`moonshot`、`sglang`、`vllm`、`ollama`、`ollama-cloud`、`huggingface`、`modelscope`、`together`、`qianfan`、`openai-codex`、`anthropic`、`openmodel`、`zai`、`stepfun`、`minimax`、`deepinfra`、`sakana`、`longcat`、`opencode-go`、`opencode-zen`、`meta`、`xai`、`mistral`、`telecomjs`、`modelstudio-token-plan`、`google`、`edenai`、`concentrate`、`codewhale` 和 `custom`(通过 `[providers.<name>]` 定义的用户自定义 OpenAI 兼容端点)。逐 provider 的 registry，包括线协议、认证变量、默认 base URL、模型 ID 和能力元数据，见 [PROVIDERS.md](PROVIDERS.md)。facade 把 provider 凭据保存到本地机密存储，并把解析后的 key、base URL、provider 和模型转发给 TUI 进程。使用 `codewhale auth set --provider nvidia-nim --api-key "YOUR_NVIDIA_API_KEY"` 或 `codewhale auth set --provider openai --api-key "YOUR_OPENAI_COMPATIBLE_API_KEY"` 或 `codewhale auth set --provider atlascloud --api-key "YOUR_ATLASCLOUD_API_KEY"` 或 `codewhale auth set --provider wanjie-ark --api-key "YOUR_WANJIE_API_KEY"` 或 `codewhale auth set --provider xiaomi-mimo --api-key "YOUR_XIAOMI_KEY"` 或 `codewhale auth set --provider fireworks --api-key "YOUR_FIREWORKS_API_KEY"` 或 `codewhale auth set --provider siliconflow --api-key "YOUR_SILICONFLOW_API_KEY"` 或 `codewhale auth set --provider arcee --api-key "YOUR_ARCEE_API_KEY"` 或 [PROVIDERS.md](PROVIDERS.md) 中匹配的 provider ID，通过 facade 保存 provider key。通用 `openai` provider 默认 `https://api.openai.com/v1`,接受 `OPENAI_BASE_URL`，默认 `gpt-5.6`。自定义 OpenAI 兼容网关仍可显式选择自己的模型。`atlascloud` 默认 `https://api.atlascloud.ai/v1`,接受 `ATLASCLOUD_BASE_URL`，默认模型是 `deepseek-ai/deepseek-v4-flash`。`wanjie-ark` 指向 Wanjie Ark 的 OpenAI 兼容端点 `https://maas-openapi.wanjiedata.com/api/v1`,默认 `deepseek-reasoner`，并原样透传模型 ID，因为 Wanjie 模型访问按账号作用域。SGLang、vLLM 和 Ollama 是自托管的，默认可以不用 API key 运行。Ollama 默认 `http://localhost:11434/v1`,并原样发送 `codewhale-coder:1.3b` 或 `qwen2.5-coder:7b` 这样的模型标签。自托管 provider 和 loopback 自定义 URL(`localhost`、`127.0.0.1`、`[::1]`、`0.0.0.0`)不读取 secret store，除非显式请求 API key 认证；当本地服务器确实需要 bearer 认证时，用环境变量或配置文件 key。Ollama Cloud 是独立的托管 `ollama-cloud` provider。它默认 `https://ollama.com/v1` 和 `gpt-oss:120b`；用 `codewhale auth set --provider ollama-cloud` 保存它的 key。环境认证先读 `OLLAMA_CLOUD_API_KEY`，然后是 Ollama 官方的 `OLLAMA_API_KEY`。SiliconFlow 默认 `https://api.siliconflow.com/v1`,接受 `SILICONFLOW_BASE_URL`，默认用 `deepseek-ai/DeepSeek-V4-Pro`。`provider = "siliconflow-CN"` 选择中国区域默认 `https://api.siliconflow.cn/v1`,配 `[providers.siliconflow_cn]` 表和 `SILICONFLOW_API_KEY` 凭据槽位。Arcee AI 默认 `https://api.arcee.ai/api/v1`,接受 `ARCEE_BASE_URL`，对 Codewhale 智能体工作默认用 `trinity-large-thinking`。`trinity-large-preview` 也作为直接的 Arcee API 模型列出；OpenRouter 的 `arcee-ai/trinity-large-thinking` 仍是 OpenRouter 命名空间形式，而直接 Arcee provider 用裸的 `trinity-large-thinking` ID。直接的 Arcee 大模型 API 调用按 256K 上下文 BF16 服务跟踪；Thinking 具备推理能力，而 Preview 未标记为推理模型。

### OpenRouter 提供商固定

OpenRouter 通过多个上游提供商（vendor）提供每个模型，Codewhale 可以用 `[providers.openrouter] vendor` 把请求固定到某个提供商（#6007）：

```toml
provider = "openrouter"
[providers.openrouter]
model = "deepseek/deepseek-v4-pro"
vendor = "deepinfra" # copy the vendor slug from the model's OpenRouter page
```

这会在 OpenRouter 请求上发送 `"provider": {"order": ["deepinfra"], "allow_fallbacks": false}`。一个基础 slug 可能匹配多个端点变体；请复制完整的 slug，例如 `deepinfra/turbo`，以选中某个变体。不可用的固定项会在 OpenRouter 处失败。Codewhale 单独的 `fallback_providers` 设置仍可在发生可恢复错误后切换整条 route。

该固定项适用于所有 OpenRouter 模型，包括该 route 上的辅助请求。设置 `vendor = ""` 可清除它。重新加载配置或重启后编辑才会生效；已在进行中的请求保留它们捕获的 route。其他 provider 不继承该固定项。`/preview-request` 会显示主请求的路由字段。

模型字符串仍然原样透传：`:floor` 按价格排序，`:nitro` 按吞吐量排序，`@preset/my-team-preset` 引用账号的预设。参见 OpenRouter 的[提供商路由](https://openrouter.ai/docs/guides/routing/provider-selection)和[预设](https://openrouter.ai/docs/guides/features/presets)文档。Codewhale 不获取各提供商的端点价格或可用性；被固定的用量会报告一个取决于路由的未知成本，而不是目录估算值。

### 自定义 OpenAI 兼容网关

对于实现 OpenAI Chat Completions API 的单个第三方服务，最简单的设置是把内置的 `openai` provider 名指向该网关：

```toml
provider = "openai"
default_text_model = "your-model-id"

[providers.openai]
api_key = "YOUR_OPENAI_COMPATIBLE_API_KEY"
base_url = "https://your-gateway.example/v1"
```

把端点放在 `[providers.openai]` 下，而不是旧的顶层 `base_url`，这样 OpenAI 兼容 provider 才能收到它。`default_text_model` 是发送给网关的模型 ID；`[providers.openai].model` 可用作 OpenAI provider 特有的覆盖。

如果你有多个 OpenAI 兼容网关，或者需要为 AgentProfile provider 固定一个稳定名称，定义一个用户命名的自定义 provider 表：

```toml
provider = "lm-studio"

[providers.lm-studio]
kind = "openai-compatible"
base_url = "http://127.0.0.1:1234/v1"
api_key = "lm-studio"
model = "qwen-2.5-7b"
```

自定义 provider 名可以用 `provider = "<name>"`、`--provider <name>` 或 AgentProfile 的 `provider = "<name>"` 选择——只要匹配的 `[providers.<name>]` 表存在。

StepFun 有一等公民的 provider 条目，所以把 Coding Plan 凭据和 base URL 保持在 `[providers.stepfun]` 作用域内：

```toml
provider = "stepfun"

[providers.stepfun]
api_key = "YOUR_STEPFUN_API_KEY"
base_url = "https://api.stepfun.ai/step_plan/v1"
model = "step-3.7-flash"
```

`/provider` 设置会问 key 属于哪个 StepFun 计费路由——按量付费(`https://api.stepfun.ai/v1`)或 Step Plan 订阅(`https://api.stepfun.ai/step_plan/v1`)——并在保存前对照你选的端点验证 key。答案被写入 `[providers.stepfun].base_url`，不会写到别处。如果该 key 已经持有 Codewhale 不识别为这两条路由之一的 base URL，问题会被跳过，你的值保持原样。

阿里云百炼 / Model Studio DashScope Qwen 路由使用同样的 OpenAI provider 形态：

```toml
provider = "openai"

[providers.openai]
api_key = "YOUR_DASHSCOPE_API_KEY"
base_url = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
model = "qwen-plus"
context_window = 1000000
```

使用与你的 API key 区域匹配的区域 DashScope `compatible-mode/v1` base URL。Codewhale 把 `qwen-plus` 保持在 `openai` provider 路由作用域内，不会从模型前缀推断出不同的 provider。同样的规则适用于所有 provider 前缀模型字符串：`deepseek-ai/...` 或 `deepseek/...` 这样的前缀是所选 provider 下的 provider 拥有线 ID，不是自动切换到 DeepSeek provider。当网关/模型的真实总上下文窗口与 Codewhale 的静态模型元数据不同时，设置 `context_window`。完整的解析顺序以及如何检查当前生效的值，见[上下文长度(context window)](#上下文长度context-window)。

如果网关接受 `POST /chat/completions` 但拒绝 `/v1/chat/completions`，设置 provider 本地的 `path_suffix`：

```toml
[providers.openai]
base_url = "https://your-gateway.example/v1"
path_suffix = "/chat/completions"
```

该后缀只应用于聊天补全请求。模型列表和 DeepSeek beta 路径保持内置路由，所以通用网关覆盖不会意外重写 `/models` 或 `/beta/completions`。

对于证书损坏或被拦截的私有网关，用 `SSL_CERT_FILE` 指向受信任的 CA 包。旧 provider 表键 `insecure_skip_tls_verify = true` 仍被解析，这样 `codewhale doctor` 能报告过期配置，但 provider 客户端会拒绝它，而不是禁用 TLS 证书验证。

Ollama、SGLang 和 vLLM 这样的本地 HTTP 端点在它们使用 localhost 或 loopback 地址时默认被允许。对于非本地的 `http://` 网关，只在受信任的网络上用 `DEEPSEEK_ALLOW_INSECURE_HTTP=1` 启动：

```bash
DEEPSEEK_ALLOW_INSECURE_HTTP=1 codewhale
```

需要额外请求头的第三方 OpenAI 兼容网关可以在顶层或 `[providers.deepseek]` 这样的 provider 表下设置 `http_headers = { "X-Model-Provider-Id" = "your-model-provider" }`。配置后，codewhale 会在模型 API 请求上发送这些自定义头。等效的环境覆盖是 `DEEPSEEK_HTTP_HEADERS`，使用逗号分隔的 `name=value` 对，例如 `X-Model-Provider-Id=your-model-provider,X-Gateway-Route=dev`。`Authorization` 和 `Content-Type` 由客户端管理，不会被此设置覆盖。

### 视觉模型(Vision Model)

Codewhale 的聊天 provider 和 `image_analyze` 工具是分开配置的。主聊天路径保持所选文本/工具 provider；图像分析在启用 `vision_model` 功能时通过 `[vision_model]` 运行。

小米当前的图像理解文档包括用于图像输入的 `mimo-v2.5`。要让 MiMo 用于 `image_analyze`，显式配置视觉模型：

```toml
[features]
vision_model = true

[vision_model]
model = "mimo-v2.5"
api_key = "YOUR_XIAOMI_KEY"
base_url = "https://api.xiaomimimo.com/v1"
```

上面的例子使用小米 MiMo 的按量付费 OpenAI 兼容端点。如果你为 `[vision_model]` 使用 Token Plan key(`tp-...`)，必须显式设置 `base_url`，因为这个通用 OpenAI 兼容块不会自动选择 MiMo 端点。新加坡账号用 `https://token-plan-sgp.xiaomimimo.com/v1`,中国区域账号用 `https://token-plan-cn.xiaomimimo.com/v1`,欧洲/阿姆斯特丹账号用 `https://token-plan-ams.xiaomimimo.com/v1`。

### 自动模型路由（`[auto]`、`[auto.router]`）

使用 `model = "auto"` 时，每个回合都运行在你**声明的默认模型**上，除非你选择加入了别的东西。Auto 绝不会根据请求的措辞去猜测该用更便宜还是更强的模型；旧的关键词加长度启发式已被移除（`crates/engine/src/model_routing.rs` 中的 `auto_route_declared_fallback`）。有两个可选的层会改变这一点：

- 由你写下的 `[auto.router]` 分类器，它逐回合挑选模型；以及
- `[auto] cost_saving`，它偏向使用活动 provider 的快速同系模型。

两者都不设置时，Auto 是本地且免费的：回合使用默认模型，不做分类器调用。

**没有默认分类器。** 不设置 `[auto.router]` 时，无论你持有哪些 key，都不会发生分类器调用。持有 DeepSeek key 曾经会自动推选 `deepseek-v4-flash`。这已被移除，因为它把 token 花在用户从未选择的路由上，并且让某个 provider 凌驾于其他所有 provider 之上（`crates/tui/src/config.rs` 中的 `AutoRouterConfig`）。选择网络分类器现在是需要你写下来的事。

用 `[auto.router]` 把分类器指向任何已配置的 provider：

```toml
[auto.router]
provider = "zai"
model = "glm-5-turbo"
thinking = "off"        # optional; defaults to off
timeout_secs = 4        # optional; default 4, 0 = default, capped at 300
```

只有当 `[auto.router]` 同时指定了 `provider` 和 `model`，**并且**该 provider 有 key 时，才会发生分类器调用：`ModelInventory::from_config`（`crates/engine/src/model_inventory.rs`）中的 `router_available = router_configured && has_api_key_for(...)`。任一条件不满足，或分类器调用出错/超时时，由本地回退决定：默认模型，或在 `cost_saving` 下的快速同系模型。回合的路由回执（回合检查器：Ctrl+Alt+O 或 `/turn inspect` 的“Model route + tokens/cost”）会记录走了哪条路径；你配置了但无法运行或失败的路由器（缺 key、HTTP 错误、超时、无效回答）会显示为 `Auto router: failing — …`，而不是被静默忽略。

#### 设置模型路由

`/router`(也可用 `/model router`)打开同一个 Router 设置视图，提供以下预设。每个预设只写入 `[auto.router]`,不会被自动选中。

| 预设 | 写入内容 | 成本与隐私 |
| --- | --- | --- |
| `/router jev` | TypeSafe 的决策模型 Jev,经由 OpenRouter(`typesafe/jev-1.13`)或 TypeSafe 直连，取决于你有哪个 key | 每回合约 $0.00002(每百万输入 token $0.042,输出免费)。你的最新请求和最多六行近期上下文会发送到 OpenRouter → TypeSafe(或 TypeSafe)。 |
| `/router fast` | 当前 provider 可运行的快速档，关闭思考 | 使用你已有的 key;分类器看到同样的请求文本。 |
| `/router off` | 删除 `[auto.router]` | 不调用路由器；Auto 回合使用默认模型，`[auto] cost_saving = true` 时使用快速档(Off 不改动该设置)。 |
| `/router custom` | 不写入；打印可手动编辑的 TOML | — |

选择 Jev 或 Fast 会用固定的示例请求做**一次测试调用**,并显示它选择的档位、概率与置信度、延迟和 provider 报告的费用。随后按 `Enter`(或 `/router save <preset>`)通过常规配置写入器保存；按 `Esc` 放弃。TypeSafe 已于 2026-09-22 暂停新用户注册，因此 OpenRouter 是新用户的默认路由。TypeSafe key 读取自 `TYPESAFE_API_KEY`、`typesafe` 密钥存储条目，或 `[providers.typesafe] api_key` / `api_key_env`。

#### 决策路由器(`kind = "decision"`)

决策路由器每回合向一个非生成式决策模型提一个有类型的问题——在当前 provider 的 `fast` 和 `strong` 档之间做 Choice,外加一个思考档位——并得到校准过的概率。不解析任何散文。

```toml
[auto.router]
kind = "decision"             # 默认 "chat"
provider = "openrouter"       # 或 "typesafe"
model = "typesafe/jev-1.13"   # 也可用 "~typesafe/jev-latest";TypeSafe 直连用 "jev-latest"
timeout_secs = 2
min_confidence = 0.5          # 默认 0.5,限制在 0..1
```

- 只有当前 provider 有可运行的 strong/fast 档位对时才会调用路由器；否则不调用、不花费。
- 置信度低于 `min_confidence` 的回答走本地回退。在 `[auto] cost_saving` 下，回答 `strong` 还需要至少 0.75 的概率，否则回合留在快速档。
- 未知的 `kind`,或决策路由器的 `provider` 不是 `openrouter` / `typesafe`,会让路由器处于未配置状态并显示为 failing。
- 决策路由器忽略 `thinking`。OpenRouter 的花费像其他路由用量一样记录；TypeSafe 直连的花费只显示在回执上。

有两个 `[auto]` 键会影响路由（`crates/tui/src/config.rs` 中的 `AutoConfig`）：

```toml
[auto]
cost_saving = false     # default false
cross_provider = false  # default false
```

- **`cost_saving`**（默认 `false`）。没有分类器时，Auto 会固定使用活动 provider 经过验证的快速同系模型，而不是默认模型。没有可运行快速同系模型的 provider 则保持默认。有分类器时，会告诉分类器：对常规或含糊的工作偏向快速档，只对明显属于智能体式、多步骤、架构、安全或调试类的工作选择强档。节省成本绝不会仅仅为了省钱而切换 provider。
- **`cross_provider`**（默认 `false`）。Auto 停留在会话配置所用的 provider 上。分类器只会看到该 provider 的模型，回退也绝不会离开它。设置 `cross_provider = true` 则允许分类器在所有可运行的 provider 中选择。`cross_provider` 没有交互式开关，必须在配置中设置。决策路由器始终在活动 provider 之内选择。

要在解析后的路径引导(bootstrap) MCP 和 skills 目录，运行 `codewhale setup`。要只搭建 MCP，运行 `codewhale mcp init`。

注意：`setup`、`doctor`、`mcp`、`features`、`sessions`、`resume`/`fork`、`exec`、`review` 和 `eval` 都可以从安装的 `codewhale` 命令获得。合并的调度器还提供 `auth`、`config`、`model`、`thread`、`sandbox`、`app-server`、`mcp-server`、`completions`、`login`/`logout`、`account`、`metrics`、`update`、`lane`、`workflow` 和 `web`。普通提示进入进程内 TUI 运行时。发布安装程序把同样的字节暴露为 `codew`。

### 启动更新检查

默认情况下，TUI 启动一个后台检查，查找最新的稳定 Codewhale release，并且只在有更新版本可用且官方发布资源完整时显示一条短 toast。该检查从不阻塞启动，从不阻塞回合，离线时静默失败。

为气隙、企业代理或受管桌面环境完全禁用启动检查：

```toml
[update]
check_for_updates = false
```

#### 节流

答案缓存在 `~/.codewhale/update-check.json`，并在 `check_interval_hours`(默认 `1`)内复用。只有*网络请求*被节流——有更新待定时，提示仍在每次启动出现。设为 `0` 表示每次启动都检查。

```toml
[update]
check_interval_hours = 1
```

失败的检查不会被缓存，所以一次故障不会在间隔过去之前抑制提示。

#### 自动抑制

以下任一值被设置为非假值(非 falsey)时，检查会被跳过，不联系网络：

| 变量 | 原因 |
| --- | --- |
| `CODEWHALE_NO_UPDATE_CHECK` | 显式选择退出。 |
| `NO_UPDATE_NOTIFIER` | 跨 CLI 惯例，为兼容性而遵循。 |
| `CI`、`CONTINUOUS_INTEGRATION`、`GITHUB_ACTIONS`、`GITLAB_CI`、`BUILDKITE`、`CIRCLECI`、`JENKINS_URL`、`TEAMCITY_VERSION`、`TF_BUILD` | 自动化构建；终端前没有人。 |

`""`、`0`、`false`、`no` 和 `off` 的值不算已设置，所以 `CI=false` 导出不会为普通用户禁用检查。

#### 提供哪条更新命令

Codewhale 从不会自己安装任何东西——它只告诉你存在更新。它命名的命令取决于运行中的二进制是如何安装的，从其路径检测：

| 安装方式 | 提供的命令 |
| --- | --- |
| GitHub release 二进制(包括 Termux) | `codewhale update` |
| npm(`node_modules` 在路径上) | `npm install -g codewhale@latest` |
| Homebrew(`Cellar` / `linuxbrew` 前缀) | `brew upgrade codewhale` |
| `cargo install`(`~/.cargo/bin`) | `cargo install codewhale-cli --locked --force` |

对于包管理器安装，提示还会警告不要用 `codewhale update`：替换 Homebrew 或 npm 拥有的二进制会让管理器描述一个磁盘上已不存在的版本，而下次升级会静默把你退回。

如果你把二进制移到了路径启发式无法读取的位置，用 `CODEWHALE_INSTALL_METHOD=npm|homebrew|cargo|binary` 覆盖检测。

要重定向启动检查，把 `update_uri` 设置为返回 GitHub 兼容 latest-release JSON 的内部端点。接受带 `tag_name` 字段的最小镜像元数据；如果存在 `assets`，Codewhale 会要求与官方 release 相同的上传资源集，然后才显示 toast。

```toml
[update]
check_for_updates = true
update_uri = "https://internal.mirror.example/codewhale/releases/latest"
```

未设置 `update_uri` 时，启动检查在回退到官方 GitHub API 端点之前，会遵循 `CODEWHALE_RELEASE_BASE_URL` 这样的 release 镜像环境变量。如果配置的 `update_uri` 无法获取或解析，且设置了 release 镜像环境变量，TUI 会回退到该镜像，而不是让启动失败。

#### 更新后的新内容提示

Codewhale 版本升级后第一次启动时，会提示一次，并链接到 codewhale.net 上该版本的发布说明：

```text
已更新到 0.10.2。包含 12 项新功能、31 项修复和 8 项其他改动。
发布说明：https://codewhale.net/zh/changelog/0.10.2
```

数量来自编译进二进制的更新日志。Added 或 New 下的条目计为新功能；Fixed、Security 和 Latest fixes 计为修复；Contributors、Thanks、Community 以及 Known limits/issues 不计入；其余各节计为其他改动。链接使用你的界面语言(中文为 `zh`)，网站没有该语言时使用英文。

TUI 在启动卡片上显示该提示，卡片离开时把它移入对话记录(从 `--resume`、`--continue` 或提示词打开会话时立即移入)，并附上 `/change <version>` 以便在应用内阅读。`doctor`、`models`、`sessions`、`receipts`、`setup`、`init`、`install`、`features`、`integrations`、`mcp`、`config`、`model`、`metrics`、`providers`、`account`、`login`、`logout` 和 `update` 命令在 stdout 和 stderr 都是终端且未请求 JSON 输出时，把它打印到 stderr。`exec`、服务器、JSON 输出和非终端运行从不显示；之后第一次能显示它的启动会显示。

全新安装或降级后不会显示任何内容。最近一次启动的版本记录在 `~/.codewhale/last-launch.json`。该提示只读取本地文件，所以与 `check_for_updates` 无关。要关闭它：

```toml
[update]
show_whats_new = false
```

## Workshop 输出预算

默认情况下，过大的工具结果采用有界溢出：低于字节阈值的结果保持内联，更大的结果给出首尾预览和一个模型可以回读的会话产物。不存在合成子智能体，也没有逐调用的 `raw = true` 逃逸。

`[workshop] large_output_threshold_tokens` 和 `[workshop.per_tool_thresholds]`(按模型可见的精确工具名，例如 `bash`)只有在进程通过 `CODEWHALE_ADAPTIVE_OUTPUT_ROUTING=1` 选择自适应证据路由时才生效。

两个可选的字节上限(#5367)在任何情况下都适用，它们提高模型可见的下限，并且永不降低：

- `read_result_max_bytes`——单个 `read` / `read_file` 结果的上限。缺省时保持编译期默认（`read` 为 100000 字节，没有行数上限；`read_file` 为 16KiB / 500 行）。对 `read` 来说，这是三层预算的中间一层：模型自己每次调用的 `max_bytes`（硬上限 500000）会为该次调用提高预算，这个设置会为整个进程提高下限，而无论哪种情况，2MiB 都是绝对上限。取最高者；任何一层都不能降低另一层已授予的预算。
- `tool_result_max_bytes`——溢出后通用工具结果的上限。缺省时保持 12K 字符的紧凑下限(窗口 ≥500K token 时为 48K)。硬上限 2MiB。

## 上下文长度(context window)

也叫上下文大小、上下文上限、最大上下文或窗口。这是 Codewhale 做预算的总 token 窗口，它驱动头部/底部上下文百分比、自动压缩触发、上下文压力检查和请求输出上限。如果 Codewhale 在你知道能服务 1M 窗口的模型上于 128K 压缩，这就是要改的设置(#5134)。

**查看当前生效的值及其来源。** 下面每一项都会打印解析后的窗口*和*它的来源：

- `/status`——`Context window:` 行显示百分比和 token 数，`Window source:` 行指出出处和确切覆盖它的键。
- `/config` → Provider——`Context window`(你的覆盖，或 `(not set)`)和 `Effective context window`(`1048576 tokens · configured`)。在 `/config` 过滤里输入 `context length` 直接跳到它们。
- `/context report`——`Window: 1048576 tokens (12.4% used, ...; source: configured)`。
- `/context json`——机器可读的 `context_window_tokens` 和 `context_window_source`。

**修改它**用 provider 表键 `context_window`：

```toml
[providers.moonshot]
context_window = 1048576
```

或从 CLI:

```bash
codewhale config set providers.moonshot.context_window 1048576
codewhale config unset providers.moonshot.context_window   # 回到自动
```

为你实际所在的 provider 使用对应表(`providers.openai`、`providers.deepseek`、`providers.moonshot`……)；`/status` 会告诉你。该值是路由*总*窗口的正 token 数。

当一个网关背后的各个模型窗口不一致时，可以用 `[providers.<name>.model_context_windows]` 把覆盖限定到某个确切的线路模型 id：

```toml
[providers.command_code]
context_window = 204800

[providers.command_code.model_context_windows]
"MiniMaxAI/MiniMax-M2.5" = 204800
"google/gemini-3.1-flash-lite" = 1000000
```

键是该路由实际发送的确切线路模型 id（带点号和 `org/model` 的写法，加引号后都可以作为 TOML 键）；每个值必须是正的 token 数。匹配的条目只对该模型胜过 provider 级的 `context_window`——该 provider 上其他每个模型仍按 `context_window` 和下面的各级来解析。从 CLI 设置：

```bash
codewhale config set 'providers.command_code.model_context_windows."MiniMaxAI/MiniMax-M2.5"' 204800
codewhale config unset 'providers.command_code.model_context_windows."MiniMaxAI/MiniMax-M2.5"'
```

### 有效窗口如何解析

首个匹配胜出，每个界面打印的来源标签就是这一级：

1. `configured (per-model)`——以该路由的确切线路模型 id 为键的 `[providers.<name>.model_context_windows]` 条目。它只对该模型是硬覆盖；绝不会改写其他模型的窗口。
2. `configured`——`config.toml` 中的 `[providers.<name>] context_window`。它对该 provider 上的每个模型都是硬覆盖：它下面的任何东西都不能升高或降低结果。读取时的别名：`contextWindow`、`context_window_tokens`、`contextWindowTokens`、`context_length`、`contextLength`。
3. `provider-reported`——provider 实际为 Kimi Code `k3` 路由报告的、限定于该路由的 1M 元数据，前提是它在最近 24 小时内被观察到。
4. `static Kimi Code safe floor`——Kimi Code 会员为 262,144 token，因为 1M 访问受套餐限制（Allegretto 及以上）。
5. `catalog`——内置的路由目录（先是人工维护的数据，然后是内置的 Models.dev 行）。官方 `openai-codex` 目录只提供当前账户的模型选择，不提供上下文窗口元数据。使用 `codewhale models --update --provider openai-codex` 刷新；这条路由不读取 `$CODEX_HOME`。
6. `model-name hint`——从模型名本身解析出的 `_Nk` 后缀（`qwen3-32b-256k` → 256,000），与厂商无关。服务引擎可能并不遵循的命名约定，并不是关于该路由的事实，所以这一级位于 catalog *之下*：同一 id 的任何 catalog 行都胜过它（#5441）。
7. `fallback`——静态的按 provider 能力表：Anthropic 线路（wire）的路由为 200,000，`openai-codex` 为 128,000，Ollama 为 8,192，否则用 Codewhale 的静态按模型元数据，模型未知时最终为 128,000。

### “(unverified)”是什么意思

`model-name hint` 和 `fallback` 这两级仍会驱动真实的预算——压缩触发、上下文计量和输出预留都使用该数字——但它们是猜测，而不是有人核实过的能力。每个渲染这类窗口的界面都会在其来源标签后追加 `(unverified)`（状态行、上下文压力消息、`/status`、`/config` 和模型选择器芯片），这样，一个你没有配置、provider 也没有报告的窗口，永远不会被读成经过验证的上限（#5239、#5441）。上面的 `context_window` 和 `model_context_windows` provider 表键就是修法：配置的窗口是硬覆盖，会渲染为 `configured`（或 `configured (per-model)`），不带标记。

输出上限也遵循这一区分（#5440）：目录未描述的 Anthropic 家族模型仍按 64K Messages 基线限制输出，但回执和选择器会把该值标为 `unverified`（或“假定基线”），而不是 `documented`。官方 ChatGPT 预览路由不发送输出上限字段；保守的内部预算不代表提供商公布的输出能力。

上下文窗口没有环境变量；provider 表的 `context_window` 和按模型的 `model_context_windows` 键就是用户旋钮。当网关或自托管运行时所服务的窗口是 Codewhale 目录没有建模的，它们就是该设置的正确选择——只有部分路由不同时按模型设置，全部不同时按 provider 整体设置。Codewhale 不会发明一个它无法自圆其说的窗口——它会回退到保守值，标为 `fallback`，并在每个显示它的界面上标记 `(unverified)`。

### 相邻旋钮

- `auto_compact_threshold_percent`（settings.toml；也接受 `auto_compact_threshold`；`10`–`100`，默认 `80`）：自动压缩触发时占完整路由上下文窗口的份额，并被钳制，确保它永远不会在输出预留和裕量之后越过可花费的输入上限。可在 `/config` 编辑。只提高窗口而不动它，会连带提高绝对压缩点。
- `auto_compact`（settings.toml，on/off）：完全关闭自动压缩；`/compact` 和 Ctrl+L 保持可用。
- `[compaction] summary_instructions` 和 `[compaction] retained_user_message_tokens`（config.toml）：常驻的摘要器指令，以及原样保留用户消息的预算。见下方键参考中的 `compaction.*` 条目。
- `CODEWHALE_MAX_OUTPUT_TOKENS`（环境变量；旧别名 `DEEPSEEK_MAX_OUTPUT_TOKENS`）：覆盖请求的输出上限。没有覆盖时，Codewhale 从安全的 `65536` 请求上限开始，并与任何更小的、有文档记录的模型或路由上限取交集；catalog 的 `max_output`（如 DeepSeek V4 的 384K）仍是能力上限，而不是每个响应请求的量。显式覆盖会在解析后的路由上下文窗口和任何路由输出上限之内被保留，预检/紧急预算预留的正是能到达线路的同一个有效值。另有单独记录的路由输入上限，即使总上下文窗口更大，也会钳制预检和压缩。空白的规范变量会回落到非空的旧变量；非空但无效或为零的规范值具有权威性，会回退到安全的自动默认值，而不是启用过期的旧设置。`config.toml` 中没有 `max_output_tokens` 键。

在压缩替换对话历史之前，Codewhale 会把原始消息持久地保存到会话的 `artifacts/context-transfer-<id>.json`，并在产生模型摘要时，把它的交接内容保存到对应的 `.md` 文件。这些使用现有的会话产物存储和持久化脱敏。写入失败会中止压缩，而不替换上下文。仅剪枝的执行会保存原始消息，不额外调用模型。模型可以通过 `context_archive_search`（ASCII 大小写不敏感的子串搜索，返回短片段和归档 id）与 `context_archive_get`（按 id 分页读取单条消息）把原文读回来；交接摘要会点名这两个工具，让模型知道归档存在。压力元数据显示估算的输入 token 和已配置的触发点；它是估算值，而不是对 provider 剩余上下文的精确承诺。

压缩历史可通过 `codewhale metrics`（或 `--json`）和 Codewhale home 中的 `audit.log` 获取。已完成的执行会记录它们的触发、摘要/剪枝路径、消息数和估算 token 数、有效阈值，以及摘要器的 token 用量。自动拒绝每回合记录一次，并附上原因。这些是本地诊断，不是 provider 的账单总额；较早的产物不会被追溯计入。尝试压缩的文本模式 `exec` 会在回合完成时保存其所属会话，使其恢复产物仍可被发现。被杀死的进程可能留下没有最终会话快照的产物；审计写入器会报告 I/O 失败，而不会捏造数据。

压缩设置见[设置文件（持久化 UI 偏好）](#设置文件持久化-ui-偏好)，每个显示的 token 数实际度量什么见 [Token 数量与驱动项](#token-数量与驱动项)。

## Profiles

你可以在同一文件里定义多个 profile:

```toml
default_text_model = "deepseek-flash"

[providers.deepseek]
api_key = "PERSONAL_KEY"

[profiles.work.providers.deepseek]
api_key = "WORK_KEY"
base_url = "https://api.deepseek.com/beta"

[profiles.nvidia-nim]
provider = "nvidia-nim"
default_text_model = "deepseek-ai/deepseek-v4-pro"

[profiles.nvidia-nim.providers.nvidia_nim]
api_key = "NVIDIA_KEY"
base_url = "https://integrate.api.nvidia.com/v1"

[profiles.fireworks]
provider = "fireworks"
default_text_model = "accounts/fireworks/models/deepseek-v4-pro"

[profiles.siliconflow]
provider = "siliconflow"
default_text_model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.siliconflow.providers.siliconflow]
base_url = "https://api.siliconflow.com/v1"

[profiles.openai-compatible]
provider = "openai"

[profiles.openai-compatible.providers.openai]
base_url = "https://openai-compatible.example/v4"
model = "glm-5"

[profiles.atlascloud]
provider = "atlascloud"

[profiles.atlascloud.providers.atlascloud]
base_url = "https://api.atlascloud.ai/v1"
model = "deepseek-ai/deepseek-v4-flash"

[profiles.sglang]
provider = "sglang"

[profiles.sglang.providers.sglang]
base_url = "http://localhost:30000/v1"
model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.vllm]
provider = "vllm"

[profiles.vllm.providers.vllm]
base_url = "http://localhost:8000/v1"
model = "deepseek-ai/DeepSeek-V4-Pro"

[profiles.ollama]
provider = "ollama"

[profiles.ollama.providers.ollama]
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:7b" # any tag `ollama list` shows

[profiles.ollama-cloud]
provider = "ollama-cloud"

[profiles.ollama-cloud.providers.ollama_cloud]
base_url = "https://ollama.com/v1"
model = "gpt-oss:120b"
```

用以下方式选择 profile:

- CLI：`codewhale --profile work`
- 环境：`DEEPSEEK_PROFILE=work`

如果选中的 profile 缺失，codewhale 会以列出可用 profiles 的错误退出。

## 旧版顶层 `base_url` 与 `api_key`

较早的版本把 DeepSeek 的端点和 key 放在 `config.toml` 的顶层，而每个读取方各自决定哪些其他路由继承它们。它们现在只存在于 provider 表中。旧文件无需改动仍可继续使用：每次加载都按同一条规则，针对每个文件和每个 `[profiles.<name>]`，把顶层键当作已经在它们所属的表中来读取：

1. `provider = "custom"` 且没有 `[providers.custom]` 表：端点、key 和模型的一份副本会变成 `[providers.custom]`。
2. 位于另一家厂商官方主机上的端点（例如 `integrate.api.nvidia.com`、`xiaomimimo.com`、`openrouter.ai` 或 ChatGPT Codex 端点）属于该厂商的表，因此 DeepSeek 路由绝不会向那里发送请求。
3. 其他一切都属于 `[providers.deepseek]`，DeepSeek-CN 也会读取它的端点和 key。

key 跟随 DeepSeek（或字面的 custom 路由）。只有当同一张表把 `provider` 设为另一家厂商、主机是该厂商自己的、并且该厂商的表没有 key 时，它才会跟随端点去往另一家厂商。没有设置 `provider` 时，NIM 主机仍会选中 `nvidia-nim`，`api.deepseeki.com` 仍会选中 `deepseek-cn`。没有自己 key 的 `[vision_model]` 继续使用顶层 key。profile 的顶层 `base_url` 现在会覆盖基础文件的 `[providers.deepseek] base_url`（此前是基础表悄悄胜出）。

当顶层值与它的表不一致时，使用表中的 `base_url` 和顶层的 `api_key`，这正是运行时此前所发送的。

加载从不改写文件。Codewhale 的下一次保存（例如 `codewhale config set`、`/config ... --save` 或 `auth set`）会移动这些键、保留注释、把旧文件的一份一次性无凭据副本写到 `config.toml.pre-migrate.bak`，并打印一行说明移动了什么。互相不一致的一对键绝不会在文件中被自行解决；两个值会一直保留，直到你做出选择，并由 `codewhale config doctor` / `codewhale doctor` 报告（只报告来源，绝不报告值）。`codewhale config get|set|unset base_url`（以及 `api_key`）作用于活动 provider 的表。

想自己整理文件时：

```bash
codewhale config migrate --dry-run            # show what would move
codewhale config migrate                      # move it (backup first)
codewhale config migrate --prefer top-level   # resolve a conflict: keep the top-level value
codewhale config migrate --prefer table       # resolve a conflict: keep the table value
```

## 环境变量

大多数运行时环境变量覆盖配置值。API key 变量在已保存配置和钥匙串凭据之后作为回退。

三个面向用户的槽位——provider、模型、base URL——暴露 `CODEWHALE_*` 别名。两种形式都设置时 `CODEWHALE_*` 值胜出；`DEEPSEEK_*` 形式为更老的 shell 保留：

- `CODEWHALE_PROVIDER`(首选)/ `DEEPSEEK_PROVIDER`(旧别名)——`deepseek|deepseek-anthropic|nvidia-nim|openai|atlascloud|wanjie-ark|volcengine|openrouter|xiaomi-mimo|novita|fireworks|siliconflow|arcee|siliconflow-CN|moonshot|sglang|vllm|ollama|ollama-cloud|huggingface|modelscope|together|qianfan|openai-codex|anthropic|openmodel|zai|stepfun|minimax|deepinfra|mistral`
- `CODEWHALE_MODEL`(首选)/ `DEEPSEEK_MODEL`(旧别名)——活动 provider 的默认模型
- `CODEWHALE_BASE_URL`(首选)/ `DEEPSEEK_BASE_URL`(旧别名)——活动 provider 的 base URL

`CODEWHALE_BASE_URL` 只应用于**活动**路由。固定到另一个 provider 的请求——子智能体或 fleet 子进程、路由工具、每回合自动路由器、选择器预览——从该 provider 自己的 `[providers.<table>]` 解析其端点，然后是它的 provider 作用域变量(`MOONSHOT_BASE_URL`、`OPENAI_BASE_URL`……)，然后是该 provider 的默认值。它从不继承活动会话的主机，而没有配置 `base_url` 的自定义路由会在 loopback 占位符上失败关闭(fail closed)，而不是借用另一个 provider 的端点。环境会把该值写入活动身份自己的表：DeepSeek-CN 回退到 `[providers.deepseek]` 取其端点和 key，但绝不会回退到环境专门指向 DeepSeek 的那个 DeepSeek 值。提供或重新选择有效路由端点的受管配置覆盖，会把通用覆盖从每条路由上拿走。

其余变量：

- `DEEPSEEK_API_KEY`
- `DEEPSEEK_ANTHROPIC_BASE_URL`
- `DEEPSEEK_HTTP_HEADERS`(自定义模型请求头，逗号分隔 `name=value` 对)
- `DEEPSEEK_DEFAULT_TEXT_MODEL`(`DEEPSEEK_MODEL` 的额外旧别名)
- `DEEPSEEK_STREAM_IDLE_TIMEOUT_SECS`(流空闲超时秒数；默认 `900`，钳制到 `1..=3600`)
- `DEEPSEEK_STREAM_OPEN_TIMEOUT_SECS`(连接建立 + 响应头等待秒数；默认 `45`，钳制到 `5..=300`；区别于每块空闲超时；设置了 `tui.stream_open_timeout_secs` 时以它为准)
- `CODEWHALE_CACHE_MAXIMAL`(`1`/`true`/`on`/`yes`)——缓存最大化上下文模式(#528)。开启时，Repo Working Set 块把顶层活动文件的**完整当前内容**物化(materialize)到每回合的系统提示中(确定性顺序，字节有界)，而不是只列出路径。这些文件未变化时该块保持字节稳定，这样 DeepSeek 的 KV 前缀缓存能持续命中；编辑文件会从该块开始缓存未命中。默认关闭(仅路径列表)。字节上限默认每文件 24 KB / 总计 96 KB。
- `NVIDIA_API_KEY` 或 `NVIDIA_NIM_API_KEY`（provider 为 `nvidia-nim` 时使用）
- `NVIDIA_NIM_BASE_URL`、`NIM_BASE_URL` 或 `NVIDIA_BASE_URL`
- `NVIDIA_NIM_MODEL`
- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_MODEL`
- `ATLASCLOUD_API_KEY`
- `ATLASCLOUD_BASE_URL`
- `ATLASCLOUD_MODEL`
- `WANJIE_ARK_API_KEY`、`WANJIE_API_KEY` 或 `WANJIE_MAAS_API_KEY`
- `WANJIE_ARK_BASE_URL`、`WANJIE_BASE_URL` 或 `WANJIE_MAAS_BASE_URL`
- `WANJIE_ARK_MODEL`、`WANJIE_MODEL` 或 `WANJIE_MAAS_MODEL`
- `VOLCENGINE_API_KEY`、`VOLCENGINE_ARK_API_KEY` 或 `ARK_API_KEY`
- `VOLCENGINE_BASE_URL`、`VOLCENGINE_ARK_BASE_URL` 或 `ARK_BASE_URL`
- `VOLCENGINE_MODEL` 或 `VOLCENGINE_ARK_MODEL`
- `OPENROUTER_API_KEY`
- `OPENROUTER_BASE_URL`
- `OPENROUTER_MODEL`
- `XIAOMI_MIMO_TOKEN_PLAN_API_KEY`、`MIMO_TOKEN_PLAN_API_KEY`、`XIAOMI_MIMO_API_KEY`、`XIAOMI_API_KEY` 或 `MIMO_API_KEY`
- `XIAOMI_MIMO_BASE_URL` 或 `MIMO_BASE_URL`
- `XIAOMI_MIMO_MODEL` 或 `MIMO_MODEL`
- `XIAOMI_MIMO_MODE` 或 `MIMO_MODE`(`token-plan-sgp`、`token-plan-cn`、`token-plan-ams` 或 `pay-as-you-go`)
- `NOVITA_API_KEY`
- `NOVITA_BASE_URL`
- `NOVITA_MODEL`
- `FIREWORKS_API_KEY`
- `FIREWORKS_BASE_URL`
- `FIREWORKS_MODEL`
- `HUGGINGFACE_API_KEY` 或 `HF_TOKEN`(`HF_TOKEN` 是 provider 为 `huggingface` 时接受的回退别名)
- `MODELSCOPE_API_KEY`
- `HUGGINGFACE_BASE_URL` 或 `HF_BASE_URL`
- `HUGGINGFACE_MODEL` 或 `HF_MODEL`
- `SILICONFLOW_API_KEY`
- `SILICONFLOW_BASE_URL`
- `SILICONFLOW_MODEL`
- `ARCEE_API_KEY`
- `ARCEE_BASE_URL`
- `ARCEE_MODEL`
- `TOGETHER_API_KEY`
- `TOGETHER_BASE_URL`
- `TOGETHER_MODEL`
- `QIANFAN_API_KEY` 或 `BAIDU_QIANFAN_API_KEY`
- `QIANFAN_BASE_URL` 或 `BAIDU_QIANFAN_BASE_URL`
- `QIANFAN_MODEL` 或 `BAIDU_QIANFAN_MODEL`
- `OPENAI_CODEX_ACCESS_TOKEN` 或 `CODEX_ACCESS_TOKEN`（旧版进程令牌；官方 ChatGPT 套餐路由忽略这些值）
- `OPENAI_CODEX_BASE_URL` 或 `CODEX_BASE_URL`
- `OPENAI_CODEX_MODEL` 或 `CODEX_MODEL`
- `OPENAI_CODEX_ACCOUNT_ID` 或 `CODEX_ACCOUNT_ID`（旧版身份提示；官方 ChatGPT 套餐路由忽略这些值）
- `CODEWHALE_CHATGPT_NEW_ACCOUNT=1`：仅在浏览器登录通过验证后，显式替换当前唯一的 ChatGPT 注册
- `ANTHROPIC_API_KEY`
- `ANTHROPIC_BASE_URL`
- `ANTHROPIC_MODEL`
- `ZAI_API_KEY` 或 `Z_AI_API_KEY`
- `ZAI_BASE_URL` 或 `Z_AI_BASE_URL`
- `ZAI_MODEL` 或 `Z_AI_MODEL`
- `STEPFUN_API_KEY` 或 `STEP_API_KEY`
- `STEPFUN_BASE_URL` 或 `STEP_BASE_URL`
- `STEPFUN_MODEL` 或 `STEP_MODEL`
- `MINIMAX_API_KEY`
- `MINIMAX_BASE_URL`
- `MINIMAX_MODEL`
- `DEEPINFRA_API_KEY` 或 `DEEPINFRA_TOKEN`
- `DEEPINFRA_BASE_URL`
- `DEEPINFRA_MODEL`
- `MISTRAL_API_KEY`
- `MISTRAL_BASE_URL`
- `MISTRAL_MODEL`
- `MOONSHOT_API_KEY` 或 `KIMI_API_KEY`
- `MOONSHOT_BASE_URL` 或 `KIMI_BASE_URL`
- `MOONSHOT_MODEL`、`KIMI_MODEL_NAME` 或 `KIMI_MODEL`
- `SGLANG_BASE_URL`
- `SGLANG_MODEL`
- `SGLANG_API_KEY`(可选；许多 localhost SGLang 服务器不需要认证)
- `VLLM_BASE_URL`
- `VLLM_MODEL`
- `VLLM_API_KEY`(可选；许多 localhost vLLM 服务器不需要认证)
- `OLLAMA_BASE_URL`
- `OLLAMA_MODEL`
- `OLLAMA_API_KEY`(可选；许多 localhost Ollama 服务器不需要认证)
- `OLLAMA_CLOUD_BASE_URL`
- `OLLAMA_CLOUD_MODEL`
- `OLLAMA_CLOUD_API_KEY`(首选的 Cloud key；`OLLAMA_API_KEY` 是官方回退)

对于下面每一个产品级 `CODEWHALE_*` 变量，匹配的旧 `DEEPSEEK_*` 名称仍作为兼容回退被读取；两者都设置时，`CODEWHALE_*` 值胜出。

- `CODEWHALE_LOG_LEVEL` 或 `RUST_LOG`(`info`/`debug`/`trace` 启用轻量详细日志)
- `CODEWHALE_SKILLS_DIR`
- `CODEWHALE_MCP_CONFIG`
- `CODEWHALE_NOTES_PATH`
- `CODEWHALE_MEMORY`(`1|on|true|yes|y|enabled` 开启用户记忆)
- `CODEWHALE_MEMORY_PATH`
- `CODEWHALE_TELEMETRY` / `DEEPSEEK_TELEMETRY`(旧别名)——匿名使用计数在当前 0.9.12 源码中默认开启，并附有点名 Codewhale 和 PostHog 的告知，以及简便且持久的退出方式。此前已明确拒绝的选择继续保持关闭。接受 `0|1|true|false|yes|no|on|off|enabled|disabled`。显式 "off" 是**底线**：它胜过 `--telemetry true` 和配置里的 `telemetry = true`，而此列表无法读取的值也解析为 off，因为杀开关(kill switch)里的拼写错误绝不能解析为 "on"。见 [`TELEMETRY.md`](TELEMETRY.md)。
- `CODEWHALE_TELEMETRY_ENDPOINT` / `DEEPSEEK_TELEMETRY_ENDPOINT`(旧别名)——`https://`,或仅对 loopback 的普通 `http://`。覆盖配置文件。未设置时选择随附默认 `https://telemetry.codewhale.net/v1/telemetry`;设置为**空字符串**会把批次路由到本地 dry-run 文件，不联系任何人。无论哪种方式，它只决定会话发送到哪里——不能覆盖选择退出。
- `CODEWHALE_ALLOW_SHELL`(`1`/`true` 启用)
- `CODEWHALE_APPROVAL_POLICY`(`on-request|untrusted|never`)
- `CODEWHALE_SANDBOX_MODE`(`read-only|workspace-write|danger-full-access|external-sandbox`)
- `CODEWHALE_NO_NEW_PRIVS`（`0`/`false`/`no`/`off`/`disabled` 选择退出）——仅 Linux。TUI 进程在启动时设置内核不可逆的 no-new-privileges 标志作为纵深防御，这会为 Codewhale 的整个进程树阻断 `sudo`/`su`/setuid 辅助程序。当启动时的沙箱模式解析为 `danger-full-access` 时，该标志已经会被跳过（#5723），所以这个变量是其余情况下的显式覆盖：如果你是 wheel 组用户，在更窄的档位下通过 Codewhale 做管理并需要提权生效，启动前把它设为假值（#5413）；或者设为真值，即使在 `danger-full-access` 下也强制开启该标志。未设置时由权限档位决定；其他启动加固（无 ptrace、无核心转储）始终保持开启。
- `CODEWHALE_MANAGED_CONFIG_PATH`
- `CODEWHALE_REQUIREMENTS_PATH`
- `CODEWHALE_MAX_SUBAGENTS`(钳制到 `1..=128`)
- `CODEWHALE_TASKS_DIR`(运行时任务队列/工件存储，默认 `~/.codewhale/tasks`，旧 `~/.deepseek/tasks` 仅在旧目录存在时回退)
- `CODEWHALE_RUNTIME_DIR`(覆盖运行时线程存储根目录)。交互式会话默认 `$CODEWHALE_HOME/sessions/<session-id>/runtime`，这样每个 Codewhale 进程拥有自己的存储(#5630)。存储是单所有者的：第二个进程在**同一**根目录上会在启动时失败。设置此变量以跨进程共享一个存储，或当运行时 API 服务器应使用稳定的非会话路径时。未设置时，API/服务器路径保持 `$CODEWHALE_HOME/tasks/runtime`。旧别名：`DEEPSEEK_RUNTIME_DIR`。
- `CODEWHALE_ALLOW_INSECURE_HTTP`(`1`/`true` 允许非本地 `http://` base URL；默认拒绝)
- `CODEWHALE_FORCE_HTTP1`(`1|true|yes|on` 把 HTTP 客户端钉到 HTTP/1.1，禁用 HTTP/2；在 Windows 或错误处理长连接 H2 流的代理后面有用)
- `CODEWHALE_HOME`(覆盖基础数据目录；默认 `~/.codewhale`)。如果你之前导出过 `DEEPSEEK_HOME`，把它改名为 `CODEWHALE_HOME`；新 Codewhale 状态路径不使用旧环境变量。
- `CODEWHALE_RELEASE_BASE_URL`(`codewhale update` 和 TUI 启动更新检查在 `[update].update_uri` 未设置时使用的 release 资源镜像，或该配置 URI 无法获取时的回退)
- `CODEWHALE_AUTOMATIONS_DIR`(覆盖自动化存储目录；默认 `~/.codewhale/automations`，旧 `~/.deepseek/automations` 仅在旧目录存在时回退)
- `NO_ANIMATIONS`(`1|true|yes|on` 在启动时强制 `low_motion = true` 和 `fancy_animations = false`，无论已保存设置如何；见 [`docs/ACCESSIBILITY.md`](./ACCESSIBILITY.md))。
- `SSL_CERT_FILE`——企业代理 / TLS 检查 MITM 用户把它指向 PEM 包(或单个 DER 证书)，证书会与平台的系统信任库一起添加。失败记录警告并继续——现有系统根仍然适用。

### 指令来源(`instructions = [...]`，#454)

添加一组额外的系统提示来源，它们按声明顺序，与自动加载的 `AGENTS.md` 拼接：

```toml
instructions = [
    "./AGENTS.md",
    "~/.codewhale/global.md",
    "~/team/agents-shared.md",
]
```

规则：

- 路径经过 `expand_path`，所以 `~` 和环境变量都可用。
- 每个文件上限 100 KiB；过大的文件会被截断并带 `[…elided]` 标记，而不是跳过。
- 缺失文件会带一条 tracing 警告被跳过，这样过期条目不会让启动失败。
- 只有用户自有的配置、profiles 和受管配置可以设置这个数组。项目配置(`<workspace>/.codewhale/config.toml`，或旧版 `<workspace>/.deepseek/config.toml`)会忽略 `instructions`，这样克隆的仓库不能挑选任意本地文件放进提示。

### Hooks

Hooks 是 **TUI 运行时功能**。它们从交互式 TUI 以及它驱动的引擎回合循环触发；`codewhale exec`、CLI 子命令、app-server / ACP 面和 `workflow` 工具不会触发它们。

[`docs/HOOKS.md`](HOOKS.md) 是全部十一个 hook 事件的权威参考——它们的触发点、环境变量、stdin 负载、超时与后台语义，以及其中哪三个可以引导（steer）Codewhale。下面的章节更深入地覆盖配置面和引导契约。

写 hook 之前值得在那里读两个契约点：

- `background = true` 意味着**提交后从不等待**。hook 仍会收到文档化的 stdin 负载和同样的超时，但它没有退出码，也无法引导。
- 引用其事件永不携带的条件的条件(`tool_call_after` / `on_error` 之外的 `exit_code` 条件，`shell_env` 上的 `mode` 条件，非工具事件上的工具条件)会在**加载时被拒绝**，记入日志，并在 `/hooks list` 中显示。它不会静默永不匹配。拒绝是逐条进行的，所以一个坏 hook 永远不会连累另一个只是共享其 `name` 或同样未命名的 hook。

### `/hooks` 列表

在 TUI 内运行 `/hooks`(或 `/hooks list`)查看每个配置的生命周期 hook，按事件分组，包括每个 hook 的名称、命令预览、有效超时和条件。设置了 `[hooks].default_timeout_secs` 时，它会替换每个逐 hook 的 `timeout_secs`，列表显示该有效值并指出覆盖来源，而不是回显逐 hook 数字。`default_timeout_secs = 0` 在加载时被拒绝——它会让配置中的每个 hook 立即过期——所以覆盖被忽略，逐 hook 的 `timeout_secs` 生效，列表显示该逐 hook 值且无覆盖出处，拒绝出现在 `configuration problems` 下。`[hooks].enabled` 标志的状态显示在顶部，这样 hooks 被全局抑制时一目了然，任何在加载时被拒绝的条目都带原因列在 `configuration problems` 下。Hooks 在 `[[hooks.hooks]]` 条目下配置——完整 schema 见 [`docs/HOOKS.md`](HOOKS.md)。

### 可变的 `message_submit` hooks

`message_submit` hooks 在提交的消息加入历史或发送给模型之前运行。与仅观察的生命周期 hooks 不同，非后台 `message_submit` hooks 可以替换或阻止提交的文本。

```toml
[[hooks.hooks]]
event = "message_submit"
command = "~/.codewhale/hooks/inject-context.sh"
timeout_secs = 2
continue_on_error = true
```

hook 在 stdin 上收到 JSON:

```json
{
  "event": "message_submit",
  "text": "original user text",
  "text_bytes": 18,
  "text_original_bytes": 18,
  "text_truncated": false,
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234
}
```

整个序列化文档上限 32 KiB。Codewhale 保留在 JSON 转义和有界元数据之后能容纳的最大 UTF-8 安全 `text` 前缀，三个 `text_*` 字段让截断显式化。即时消息、恢复的队列条目、合并的引导消息（steer）和先前 hook 的替换都穿过同样的序列化边界。

如果 hook 以 `0` 退出并打印带非空字符串 `text` 字段的 JSON，该值会替换提交的文本：

```json
{ "text": "replacement user text" }
```

以 `0` 退出但 stdout 为空，或 stdout JSON 没有 `text`，则当前文本保持不变。JSON `text` 字段不能为空；`{"text":""}` 被视为无效 stdout 并忽略。以 `2` 退出会在回合开始前阻止提交；结构化 `reason` 字段可以提供 TUI 中显示的、有界且脱敏的状态消息。原始 stdout、stderr 和进程错误文本不会复制进拒绝回执。其他非零退出遵循 hook 的 `continue_on_error` 设置。超时和派生失败也会在 `continue_on_error = true` 允许提交继续时，以瞬态 TUI 状态消息的形式浮现。

多个 `message_submit` hooks 按配置顺序运行，每个 hook 收到前一个 hook 产生的文本。标记为 `background = true` 的 hooks 是仅观察的，不能转换或阻止消息——它们仍收到同样的 stdin 负载和同样的环境，只是从不被等待。现有环境变量保持可用。`shell_env` hooks 保持它们现有的 `KEY=VALUE` stdout 契约；JSON stdout 契约存在于 `message_submit`(上面)和 `tool_call_before`(下面)。

### `tool_call_before` 决策 hooks

`tool_call_before` hooks 在每次工具调用执行之前运行。除了旧的硬拒绝(退出码 `2`，无论 stdout 如何总是胜出)，前台 hook 可以以退出码 `0` 在 stdout 上打印 JSON 决策：

```json
{
  "decision": "allow" | "deny" | "ask",
  "reason": "human-readable explanation (used for deny)",
  "updatedInput": { "command": "ls -la" },
  "additionalContext": "text appended to the tool result for the model"
}
```

所有字段都是可选的。空 stdout、非 JSON stdout 和没有 `decision` 字段的 JSON 行为与以前完全一样(allow)。无法识别的 `decision` 字符串会记录一条固定警告，不回显不可信值，并按 allow 处理。

- `deny` 阻止工具；模型收到包含 `reason` 的权限拒绝工具结果。
- `ask` 在 Ask 和 Auto-Review 中强制交互式审批提示，即使是对本会自动运行的工具。Full Access 不打开工具审批提示，所以 hook 的 `ask` 不会降级该档位。
- `updatedInput` 必须是 JSON 对象；它在执行前替换工具输入。多个 hook 提供时，最后一个 hook 胜出。
- `additionalContext` 作为 `[hook context] ...` 追加到发回模型的工具结果。多个 hook 的上下文会拼接。

多个 hook 匹配时，优先级是 deny > ask > allow。标记为 `background = true` 的 hooks 不能引导工具调用——它们被提交且从不等待，所以没有裁决可贡献。

一个完全没有产生裁决的前台 hook——它撞上超时、进程无法启动，或严格进程以非零退出且没有显式 JSON 决策——不会被当作许可。如果*那个* hook 配置了 `continue_on_error = false`，结果会拒绝工具调用，拒绝会指名 hook 和有界原因。严格性是从实际匹配这次调用的 hooks 上读取的，所以作用域在另一个工具上的严格门禁不能拒绝它，而宽松 hook 的超时也不会仅仅因为配置里别处存在严格 hook 就拒绝。在默认的 `continue_on_error = true` 下，结果被记录，调用继续。

`reason` 和 `additionalContext` 有上限(每个字段 2,000 字符，一次调用的拼接上下文 8,000)，并且在到达 TUI 或模型之前剥离控制字符。

拒绝 hook 示例：

```toml
[[hooks.hooks]]
event = "tool_call_before"
command = '''echo '{"decision":"deny","reason":"blocked by project policy"}' '''
condition = { type = "tool_name", name = "exec_shell" }
```

ask hook 示例(为每个 MCP 工具强制审批):

```toml
[[hooks.hooks]]
event = "tool_call_before"
command = '''echo '{"decision":"ask"}' '''
condition = { type = "tool_name", name = "mcp__*" }
```

输入重写示例：

```toml
[[hooks.hooks]]
event = "tool_call_before"
command = "~/.codewhale/hooks/clamp-shell-timeout.sh"
condition = { type = "tool_name", name = "exec_shell" }
```

其中脚本读取 hook 上下文，然后用调整后的参数打印 `{"updatedInput": {...}}`。

`tool_name` 条件支持 `*` glob：`mcp__*` 匹配每个 MCP 工具(例如 `mcp__github__create_issue`)但不匹配 `read_file` 这样的内置工具；精确名称保持精确匹配。模式中的其他正则元字符按字面匹配。

### 项目本地 hooks

仓库可以在 `<workspace>/.codewhale/hooks.toml` 中携带策略，使用与 `[hooks]` 表相同的形态(顶层字段加 `[[hooks]]` 条目)。项目 hooks 是可执行的 shell 配置，所以 Codewhale 只会在工作区通过信任提示或 `[projects."<workspace>"] trust_level = "trusted"` 条目在用户自有配置中被信任之后才加载它们。会话 `/trust on` 模式本身不启用仓库提供的 hooks，仓库本地旧标记(如 `.deepseek/trusted`)也不启用项目 hooks。一旦受信任，项目 hooks 会追加到 `config.toml` 的全局 hooks 之后，所以它们最后运行，并且对于 `updatedInput`，在平局时胜出。格式错误的受信任项目文件记录警告，启动回退到仅全局 hooks。

```toml
# .codewhale/hooks.toml
[[hooks]]
event = "tool_call_before"
command = '''echo '{"decision":"deny","reason":"no shell in this repo"}' '''
condition = { type = "tool_name", name = "exec_shell" }
```

### 回合结束观察者 hooks

`turn_end` hooks 在每个模型回合结束后观察结束状态，此时回合后状态、用量总计、成本核算、通知、回执和队列恢复都已更新。它们在 stdin 上收到 JSON，并且是仅观察的：stdout 被忽略，失败记录为警告，hook 不能阻止用户输入、修改转录或改变下一个排队的后续动作。

仅观察的 UI 事件共享一个 32 条目队列和两个持久 worker；终端循环使用非阻塞提交，不为每个事件创建线程。队列满或调度器不可用会丢弃那个观察者事件，并保留为事件特有的错误 toast，独立于普通的 agent/回合状态文本。

```toml
[[hooks.hooks]]
event = "turn_end"
command = "~/.codewhale/hooks/turn-audit.sh"
timeout_secs = 2
continue_on_error = true
```

负载包含通用 hook 元数据加回合后核算：

```json
{
  "event": "turn_end",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "created_at": "2026-07-12T10:30:00+00:00",
  "model_backed": true,
  "provider": "deepseek",
  "model": "deepseek-chat",
  "billing_surface": null,
  "turn_id": "turn_12345678",
  "status": "completed",
  "error": null,
  "duration_ms": 1834,
  "usage": {
    "input_tokens": 1200,
    "output_tokens": 180,
    "prompt_cache_hit_tokens": 900,
    "prompt_cache_miss_tokens": 300,
    "prompt_cache_write_tokens": 0,
    "reasoning_tokens": null,
    "reasoning_replay_tokens": null
  },
  "totals": {
    "session_tokens": 1380,
    "conversation_tokens": 1380,
    "input_tokens": 1200,
    "output_tokens": 180
  },
  "tool_count": 2,
  "queued_message_count": 1,
  "stop_hook_active": false
}
```

`created_at` 锚定时间窗口定价；`provider` 和 `model` 标识模型支撑回合使用的有效路由。`billing_surface` 是从实际服务该回合的端点派生的可选、非机密分类。可识别的 StepFun 路由发出 `stepfun-payg` 或 `stepfun-plan`；原始 base URL 从不写入 hook 或运行时记录。运行时 `TurnRecord` 导出把同一字段称为 `effective_billing_surface`，`scorecard` 接受它作为别名。这让订阅配额与 token 定价用量分开。无法识别和自定义端点保持 `null` 且不计价。

仅 shell 的生命周期完成把 `model_backed` 设为 `false`，并可能报告 `null` provider；离线 scorecard 从模型 token 和成本总计中排除这些记录。没有匹配 `TurnStarted` 的仅完成 shell、手动压缩和 purge 事件，会保留带合成 `lifecycle_<uuid>` 回合 id 和完成被观察时间的观察者通知。

对于 `interrupted` 或 `failed` 回合，`status` 反映该终态，`error` 在可用时携带引擎错误字符串。`stop_hook_active` 保留用于未来的重入保护，目前总是 `false`。

### 子智能体生命周期 hooks

`subagent_spawn` 和 `subagent_complete` hooks 观察子智能体生命周期事件。它们在 stdin 上收到有界 JSON 元数据，并且是仅观察的：hook 失败记录为警告，不阻止子智能体调度、不改变提示，也不改变结果。对于这些观察者事件，`continue_on_error` 没有效果：即使前面的 hook 非零退出，后面的匹配 hooks 仍会运行。

```toml
[[hooks.hooks]]
event = "subagent_complete"
command = "~/.codewhale/hooks/subagent-audit.sh"
timeout_secs = 2
continue_on_error = true
```

`subagent_spawn` 收到：

```json
{
  "event": "subagent_spawn",
  "agent_id": "agent_12345678",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234,
  "prompt_preview": "bounded prompt preview",
  "prompt_truncated": false
}
```

`subagent_complete` 收到同样的通用字段加终端元数据：

```json
{
  "event": "subagent_complete",
  "agent_id": "agent_12345678",
  "session_id": "sess_12345678",
  "workspace": "/path/to/workspace",
  "mode": "agent",
  "model": "deepseek-chat",
  "total_tokens": 1234,
  "status": "completed",
  "result_preview": "bounded result preview",
  "result_truncated": false
}
```

预览在交付前被限制，所以生命周期 hooks 不会收到完整的子智能体提示、转录或无界结果。需要完整子智能体细节时，用 `agent` 返回的转录句柄。

### 运行中回合的输入

输入区快捷键在整个会话中保持同样的角色：

- **Enter** 空闲时发送，繁忙时排队一个下一回合后续动作。该行为在 provider 首个 token 之前与之后不变。
- 输入区为空且可见排队后续动作时，**Enter** 现在把最旧的排队后续动作送入活动回合。
- **Ctrl+Enter**(或终端转发它时的 **Cmd+Enter**)显式引导（steer）活动回合。空闲时它正常发送。
- 默认情况下，**Shift+Enter**、**Alt+Enter** 和 **Ctrl+J** 插入换行。
- 设置 `composer_multiline_mode = true` 让 **Enter** 插入换行、**Alt+Enter** 发送。终端能够区分 **Shift+Enter** 时，它也会发送；传统终端可能将它报告为普通 **Enter**。**Ctrl+J** 始终插入换行，受支持的 **Ctrl+Enter** / **Cmd+Enter** 行为不变。
- **Ctrl+G** 和 **Ctrl+S** 只暂存草稿；它们从不发送或引导。

### 输入区暂存(`/stash`，Ctrl+G / Ctrl+S)

在输入区按 **Ctrl+G** 把当前草稿停放到 `~/.codewhale/composer_stash.jsonl`。`/stash list` 显示停放的草稿，带单行预览和时间戳；`/stash pop` 恢复最近停放的草稿(LIFO)；`/stash clear` 清空文件。上限 200 条目；多行草稿完整往返。**Ctrl+S** 在转发它的终端中仍是别名；Cursor 和 VS Code 把 Ctrl+S 保留给 Save，所以 Ctrl+G 是可移植默认。

## 设置文件(持久化 UI 偏好)

codewhale 还把用户偏好存储在：

- 新安装：`~/.codewhale/settings.toml`
- 存在旧设置文件时：`~/.deepseek/settings.toml` 或旧平台配置目录 `deepseek/settings.toml`

值得注意的设置包括 `auto_compact`，它对已知上下文窗口(直到 1M-token V4 类)使用模型感知的默认开启策略。自动压缩在活动模型上限之前运行，并把压缩后的摘要带入下一个请求。触发默认是 `auto_compact_threshold_percent = 80`。喜欢手动连续的可以持久化 `auto_compact = false`；手动 `/compact` / Ctrl+L 保持可用。你可以用 TUI 里的 `/settings` 和 `/config`(交互式编辑器)检查或更新这些设置。

常用设置键：

- `theme`（`system`、`terminal`、`underwater`、`underwater-retro`、`shoreline`、`shoreline-light`、`dark`、`light`、`grayscale`、`catppuccin-mocha`、`tokyo-night`、`dracula`、`gruvbox-dark`、`claude`、`matrix`、`solarized-light`、`uwu`；默认 `underwater`）：`underwater` 是全新安装时默认的深海军蓝主题，`shoreline` 是它的暖炭色替代，`shoreline-light` 是其浅色变体，`system` 跟随终端背景检测，`dark`/`light` 使用 Codewhale Whale 配对，`terminal` 继承宿主终端，`grayscale` 是低意见的黑/白主题，命名的社区预设应用于整个 TUI。`whale`、`mono`、`black-white`、`tokyonight` 和 `gruvbox` 这样的别名被接受。在 Whale 中，钴蓝色拥有动作/焦点，海沫绿拥有实时工作，Signal Gold 拥有人类决策和鲸鱼，珊瑚色拥有警告，玫瑰色拥有危险，紫色拥有 Operate，绿色保持已完成/已验证。文本标签、标记和动效策略在颜色不可用时携带同样的状态；颜色从来不是唯一的线索。用户创作的覆盖只存在于 `~/.codewhale/themes/<name>.json`(或 `$CODEWHALE_HOME/themes/<name>.json`)，用 `/theme custom:<name>` 选择。文件名是有界的 slug，符号链接和超过 64 KiB 的文件被拒绝，颜色必须是 `#RRGGBB`，未知字段会验证失败。`/theme schema` 打印嵌入的 JSON Schema，`/theme path` 显示确切目录。覆盖命名一个编译好的 `base` 主题，只改变列出的语义颜色；它不能包含或读取另一个文件。打开 `/theme` 可浏览有效的覆盖、实时预览它们，并且在打开选择器后没有移动选择时，保持当前活动的 `custom:<name>` 选择器。
- `auto_compact`(on/off，模型感知默认对已知上下文窗口开启，除非显式配置)
- `auto_compact_threshold_percent`(10-100，默认 `80`)：仅当 `auto_compact` 启用时使用的发送前自动压缩阈值。
- `paste_burst_detection`(on/off，默认 on)：为不发出括号粘贴事件的终端提供的快速按键粘贴回退检测。这独立于终端的括号粘贴模式。
- `work_surface_placement`（`bottom`、`top`、`left`、`right` 或 `off`；默认 `bottom`）：把任务面板——任务 / 待办 / Workers——放在输入区下方（默认的底部面板）、转录上方、侧边面板，或完全隐藏（`off`）。在窄终端上，侧边选择会回退到顶部布局，而不改变已保存的偏好。用 `/config work_surface_placement right --save`（或 `left` / `top` / `bottom` / `off`）实时设置。
- `rail_panel`（`tasks`、`agents`、`background`、`files`、`notepad`、`context`、`git`、`price`；默认 `tasks`，别名键 `rail`）：任务面板显示哪个面板。面板选择与放置位置相互正交。`tasks` 是完整的实时工作列表（先是待办，然后是子智能体）；`agents` 收窄到子智能体行；`background` 列出后台 shell 和自动化；`files` 列出被触碰的文件；`notepad` 显示工作区笔记；`context` 是只读的会话事实列表；`git` 显示分支状态；`price` 显示成本。除 `context` 外的每个面板中，行都可选择、可点击，并会打开它们的详情界面。`Alt+!`/`Alt+@`/`Alt+#`/`Alt+$` 实时切换面板。
- `work_surface_top_height`（2–16）和 `work_surface_side_width`（26–80）：顶部条带高度和侧边任务面板宽度的上限。两者通常通过拖动分隔线来持久化，而不是手工编辑；条带在上限之下仍会自动适配其内容。
- `focus_texture`(`off`、`scrim` 或 `grain`；默认 `off`)：模态视图的焦点上下文纹理。`scrim` 把聚焦模态之外的已渲染背景朝主题表面调暗；`grain` 在空白单元格上撒稀疏点。纹理是静态的(没有时间分量，所以不受 `low_motion` 影响)，从不覆盖带文本的单元格，并在两种颜色都可解析的地方保持 4.5:1 的正文对比度底线。在低于环境活跃最小尺寸的帧上，以及聚焦模态已覆盖 90% 及以上帧时，它被完全跳过。用 `/config focus_texture scrim --save` 实时设置。
- `mention_menu_limit`(整数，默认 `128`)：输入区渲染可见窗口之前保留的 `@`-提及弹出候选最大数。可见行仍取决于终端高度。
- `mention_walk_depth`(整数，默认 `10`)：`@`-提及补全遍历的最大工作区深度。在深度嵌套的工作区设为 `0` 表示无限深度；在非常大的仓库保持默认，除非需要。
- `mention_menu_behavior`(`fuzzy`、`browser`；默认 `fuzzy`)：控制 `@`-提及补全如何填充。`fuzzy` 搜索工作区并应用提及 frecency。`browser` 只按确定性字母顺序列出当前输入目录段的直接子项。
- `show_thinking`(on/off)
- `thinking_default_expanded`（on/off，默认 off）：启用 `show_thinking` 时，让思考块初始以展开状态渲染。Space 仍可切换所选块，并且它只决定你尚未动过的块：你自己展开或折叠过的块，即使之后更改这个设置，也会保持该状态。这在 Space 绑定可能被拦截的 SSH/tmux 环境中很有用。
- `thinking_preview_lines`(整数，默认 `2`):**折叠**的已完成思考仍显示多少正文行。`0` 仅头部；`10` 是较旧的完整显示。实时流式预览不变。用 Space 展开块，或设置 `thinking_default_expanded` 打开每个块。
- `help_expand_groups`(on/off，默认 off)：让帮助/快捷键从每个组展开开始。默认折叠长尾(Grok 风格)；输入过滤仍展开匹配。
- `pin_last_prompt`(on/off，默认 on)：在最后一条用户提示滚出后，把它固定在转录视口顶部。
- `show_tool_details`(on/off)
- `inline_diffs`(`full`、`summary` 或 `off`；默认 `full`)：控制成功的结构化文件变更的内联呈现。`full` 显示有界的红/绿 diff 和语义统计，`summary` 只保留统计，`off` 保持平静的变更文件结果。三种都保留所选 File 回执的 Alt/Option+V 详情中的确切已应用变更。失败和取消从不渲染成功 diff。用 `/config inline_diffs <mode> --save` 保存选择。
- `locale`(`auto`、`en`、`ja`、`zh-Hans`、`zh-Hant`、`pt-BR`、`es-419`、`vi`、`ko`；默认 `auto`):UI chrome 区域设置。`auto` 检查 `LC_ALL`、`LC_MESSAGES`，然后 `LANG`；不支持的区域设置选择解析为英语。每个随附的语言包都持有完整的 `en.json` 对等，所以没有字符串回退到英语。运行时还在系统提示中把解析出的区域设置作为 V4 推理和回复的回退自然语言暴露，当最新用户消息有歧义时。清晰用户语言仍优先；中文回合应产生中文 `reasoning_content` 和中文最终回复，即使解析出的区域设置是英语。
- `background_color`(`#RRGGBB`、`RRGGBB` 或 `default`)：可选的主 TUI 背景色，应用于根、头部、转录和底部表面，同时保留面板对比。
- `cost_currency`(`usd`、`cny`；默认 `usd`)：底部栏、上下文面板、`/cost`、`/tokens` 和长回合通知摘要使用的货币。别名 `rmb` 和 `yuan` 规范化为 `cny`。
- `default_mode`(`agent`、`plan` 或 `operate`；旧值为了迁移被接受，但不是活动模式词汇)
- `launch_screen`（旧版，仅用于迁移）：这个历史上的 `on`/`off` 值在读取现有设置文件时仍会被接受，但不再改变任何行为，并且新保存时会被省略。全新的交互式启动总是打开 Tideline Startup；只有显式的恢复或显式的初始提示才会直接进入实时会话。
- `sidebar_focus`(旧版，仅迁移)：这个键配置的经典右侧栏已在 0.9.4 rail 统一中移除。该键仍被读取一次，让旧设置向前迁移，然后折叠进实时键：`pinned`/`work`/`plan`/`todos` 变成 `rail_panel = "pinned"`，`agents`/`subagents` 变成 `rail_panel = "agents"`，`context`/`session` 变成 `rail_panel = "context"`，`tasks`/`auto`(旧默认)变成 `tasks` 面板，`sessions` 启用 `sessions_rail`，`hidden` 通过 `work_surface_placement = "off"` 关闭任务面板。文件中的显式 `rail_panel` 总是胜过迁移值。用 `rail_panel` 和 `work_surface_placement` 配置任务面板，不要用这个键。
- `sessions_rail`（`on`/`off`；默认 `off`）：在任务面板中显示持久的 Sessions 列表。行列出该工作区最近的未归档会话，新的在前，并标出活动的那个；激活一行会打开预先选中它的会话选择器（`/sessions open <id>`），这样恢复功能只有一份实现。行是从缓存的会话元数据投影出来的——该列表从不逐帧读取转录，也从不联系 provider。
- `session_auto_resume`(`on`/`off`；默认 `off`):Codewhale 启动时重新附加到该工作区最近的会话。默认关闭，所以普通的 `codewhale` 保持全新启动。`--resume`、`--continue` 和 `--fresh` 总是优先。开启时，启动仍拒绝恢复已归档、加载失败或记录在另一个工作区的会话；每个都回退到全新转录，并说明跳过了哪个会话及原因。它只适用于交互式启动——`codewhale "<prompt>"` 和 `codewhale exec` 从不被静默加上先前对话前缀。
- `max_input_history`(已提交输入历史条目数；清除的草稿也保留在本地供输入区历史搜索)。注意拼写：磁盘上的 serde 字段是 `max_input_history`(`crates/tui/src/settings.rs:426`，默认 100)。`max_history` 是 `/config set` 和 `settings.set()`(`settings.rs:1388`)接受的键名，不是 settings.toml 键——把 `max_history` 写进文件会被静默忽略。
- `default_model`(模型名覆盖)

`/task digest`(别名 `/tasks digest`)以纯文本渲染规范的 Work Graph 操作和四状态待办列表，运行中的工作优先。它读取与样式化 Work 面相同的快照，不拥有并行的进度状态。

Plan 和 Work 是 UI 中的日常可见模式；Operate 是显式预览入口，其 Workflow 控制面仍在构建中。用 `/mode` 切换。为了兼容，较旧设置文件中的 `default_mode = "normal"` 仍作为 `agent` 加载。

本地化范围在 [LOCALIZATION.md](./LOCALIZATION.md) 中跟踪。v0.7.6 核心包只覆盖高可见性 TUI chrome;provider/工具 schema、个性提示和完整文档保持英语，除非之后显式翻译。

可读性语义：

- 选择在转录、输入区菜单和模态框中使用统一样式。
- 底部提示使用专用语义角色(`FOOTER_HINT`)，这样提示文本跨主题保持可读。

### Token 数量与驱动项

DeepSeek V4 前缀缓存让 token 标签变得重要。这些数量保持分离：

| 数量 | 含义 | 允许驱动 |
|---|---|---|
| 活动请求输入估算 | 下一个请求实时系统提示和转录负载的保守估算。 | 头部/底部上下文百分比、自动压缩触发、选择加入 Flash seam 触发、紧急溢出预检。 |
| 预留响应裕量 | 有效请求上限加 `1024` 安全 token。常规无覆盖请求从 `65536` 开始；更小的路由/provider 上限收窄该值，显式输出覆盖只在解析后的路由窗口和输出上限内提高它。同一上限到达线路并驱动预检；例外是声明的输出上限超过已知上下文窗口一半且没有显式输出覆盖的路由：压缩以保留窗口的四分之一为目标（最多 `65536`，且不超过请求上限），预检只在剩余输出不足 `128` token 时拒绝请求，每个请求的 `max_tokens` 是窗口减去该请求的估计输入和 `1024` 安全 token 后剩余的部分，且不超过请求上限。思考强度不会增加第二个隐藏预留。单独发布的路由输入上限独立钳制可花费输入预算。 | 仅紧急溢出预算检查。 |
| 累计 API 用量 | provider 报告的输入加输出 token 跨已完成 API 调用求和；多工具回合可能多次计入同一稳定前缀。 | 仅会话用量和近似成本遥测。 |
| 提示缓存命中/未命中 | 可用时最近一次调用的 provider 缓存遥测。 | 仅缓存命中显示和成本估算；从不触发压缩或 seam。 |
| 上下文百分比 | 活动请求输入估算除以模型上下文窗口。 | 仅显示；它镜像上下文防护所用的活动输入基础。 |
| 成本估算 | 来自 provider 用量和已配置 DeepSeek 费率的近似花费。 | 仅显示。 |

对于已知上下文窗口的模型(包括 1M 类 V4 模型)，替换式压缩默认启用，除非用户显式配置 `auto_compact = false`。它在活动模型的压缩阈值触发，用最近的用户上下文加一条普通检查点消息替换旧历史。常驻系统提示保持不变。未知模型 id 保持选择加入。

### 命令迁移说明

如果你从更旧的版本升级：

- 旧：`/codewhale` 新：`/links`(别名：`/dashboard`、`/api`)
- 旧：`/set model deepseek-reasoner` 新：`/config` 并把 `model` 行编辑为 `deepseek-v4-pro` 或 `deepseek-flash`
- 旧：可见 `Normal` 模式或 `default_mode = "normal"` 新：使用 `Agent` / `default_mode = "agent"`；旧 `normal` 仍映射到 `agent`
- 旧：在斜杠 UX/帮助中发现 `/set` 新：编辑用 `/config`，只读检查用 `/settings`

## 键参考(Key Reference)

### Kimi Code 会员模型 ID

精确的 `https://api.kimi.com/coding/v1` 端点接受 `k3`、`k3-256k`、`kimi-for-coding` 和 `kimi-for-coding-highspeed`。用 `k3-256k` 获得固定的 262,144-token K3 窗口；只有在会员计划包含 1M 资格(entitlement)时才用裸 `k3` 搭配 `context_window = 1048576`。两个 K3 id 使用相同的推理契约，且四个会员 id 都省略通用采样字段。

### 核心键（供 TUI/引擎使用）

- `provider`(字符串，可选)：`deepseek`(默认)、`deepseek-anthropic`、`nvidia-nim`、`openai`、`atlascloud`、`wanjie-ark`、`volcengine`、`openrouter`、`xiaomi-mimo`、`novita`、`fireworks`、`siliconflow`、`arcee`、`siliconflow-CN`、`moonshot`、`sglang`、`vllm`、`ollama`、`ollama-cloud`、`huggingface`、`modelscope`、`together`、`qianfan`、`openai-codex`、`anthropic`、`openmodel`、`zai`、`stepfun`、`minimax`、`deepinfra`、`sakana`、`longcat`、`opencode-go`、`meta`、`mistral`、`telecomjs`、`xai`、`orcarouter`、`modelstudio-token-plan`、`google`、`edenai` 或 `custom`。旧 `deepseek-cn` 配置仍作为 `deepseek` 的别名被接受；DeepSeek 在全球使用同一个官方主机 [`https://api.deepseek.com`](https://api-docs.deepseek.com/)。`deepseek-anthropic` 使用 `DEEPSEEK_API_KEY` 指向 DeepSeek 的 Anthropic Messages 兼容端点 `https://api.deepseek.com/anthropic`;`nvidia-nim` 通过 `https://integrate.api.nvidia.com/v1` 指向 NVIDIA NIM 托管的 DeepSeek 端点；`openai` 指向通用 OpenAI 兼容端点，默认 `https://api.openai.com/v1`;`atlascloud` 指向 AtlasCloud 的 OpenAI 兼容端点 `https://api.atlascloud.ai/v1`;`wanjie-ark` 指向 Wanjie Ark 的 OpenAI 兼容端点 `https://maas-openapi.wanjiedata.com/api/v1`;`volcengine` 指向火山方舟(Volcengine Ark)的 OpenAI 兼容编码端点 `https://ark.cn-beijing.volces.com/api/coding/v3`;`openrouter` 指向 `https://openrouter.ai/api/v1`;`xiaomi-mimo` 指向小米 MiMo 的 OpenAI 兼容端点，Token Plan key(`tp-...`)默认用 `https://token-plan-sgp.xiaomimimo.com/v1`,按量付费 key 用 `https://api.xiaomimimo.com/v1`。对于新加坡默认之外的 Token Plan 账号，显式设置 `base_url` 或对中国的账号用 `mode = "token-plan-cn"`，对欧洲/阿姆斯特丹用 `mode = "token-plan-ams"`；`novita` 指向 `https://api.novita.ai/openai/v1`;`fireworks` 指向 `https://api.fireworks.ai/inference/v1`;`siliconflow` 指向 SiliconFlow，默认 `https://api.siliconflow.com/v1`;`arcee` 指向 Arcee AI 的 OpenAI 兼容端点 `https://api.arcee.ai/api/v1`;`siliconflow-CN` 通过 `[providers.siliconflow_cn]` 指向 SiliconFlow 中国区域端点；`moonshot` 指向 Moonshot/Kimi，默认 `https://api.moonshot.ai/v1`;`sglang` 指向自托管 OpenAI 兼容端点，默认 `http://localhost:30000/v1`;`vllm` 指向自托管 vLLM OpenAI 兼容端点，默认 `http://localhost:8000/v1`;`ollama` 指向 Ollama 的 OpenAI 兼容端点，默认 `http://localhost:11434/v1`;`huggingface` 指向 Hugging Face Inference Providers `https://router.huggingface.co/v1`;`modelscope` 指向魔搭社区的 OpenAI 兼容推理 API `https://api-inference.modelscope.cn/v1`;`together` 指向 Together AI `https://api.together.xyz/v1`;`qianfan` 指向百度千帆 `https://api.baiduqianfan.ai/v1`;`openai-codex` 通过官方 ChatGPT 登录访问公共 API `https://api.openai.com/v1`；`anthropic` 指向 Claude 的原生 Messages API；`openmodel` 指向 OpenModel 的 Anthropic 兼容 Messages API `https://api.openmodel.ai`;`zai` 指向 Z.ai `https://api.z.ai/api/coding/paas/v4`;`stepfun` 指向 StepFun `https://api.stepfun.ai/v1`;`minimax` 指向 MiniMax `https://api.minimax.io/v1`;`deepinfra` 指向 DeepInfra `https://api.deepinfra.com/v1/openai`;`sakana` 指向 Sakana AI Fugu `https://api.sakana.ai/v1`;`longcat` 指向美团 LongCat `https://api.longcat.chat/openai/v1`;`opencode-go` 指向订阅支撑的、感知模型的 OpenCode Go 路由（按文档记载的模型使用 Chat Completions、Responses 或 Messages）`https://opencode.ai/zen/go/v1`;`meta` 指向 Meta Model API；`mistral` 指向 Mistral AI 的 OpenAI 兼容端点 `https://api.mistral.ai/v1`;`telecomjs` 指向 TelecomJS TokenHub `https://aigw.telecomjs.com/v1`;`xai` 指向 xAI 的 API key 或 OAuth 路由。

- `opencode-zen`(字符串 provider 值)：通过 `[providers.opencode_zen]` 选择模型感知的 OpenCode Zen 网关。默认 base URL 是 `https://opencode.ai/zen/v1`,默认模型 `gpt-5.6`，凭据来自 `api_key`、`OPENCODE_ZEN_API_KEY` 或回退 `OPENCODE_API_KEY`——绝不是 ChatGPT/Codex OAuth。接受 `OPENCODE_ZEN_BASE_URL` 和 `OPENCODE_ZEN_MODEL`。所选模型的线协议先取自精选的 Zen 快照，再取自其 Models.dev `opencode` 行声明的 AI SDK 包（用 `codewhale models --update` 刷新）：GPT、Grok 和 Muse Spark 用 Responses,Claude 和大多数 Qwen 行用 Anthropic Messages,DeepSeek/MiniMax/GLM/Kimi、`qwen3.8-max` 和免费行用 Chat Completions。Gemini、Models.dev 标记为 `deprecated` 的行以及任何已加载目录都未列出的模型失败关闭，因为 Codewhale 对它们没有经过验证的受支持线契约。确切当前模型组见 [`PROVIDERS.md`](PROVIDERS.md#opencode-zen-协议目录)。

- `minimax-anthropic`(字符串 provider 值)：通过 `[providers.minimax_anthropic]` 选择 MiniMax 的 Anthropic 兼容 Messages 路由。默认 Base URL 是 `https://api.minimax.io/anthropic`;中国区域设置 `https://api.minimaxi.com/anthropic`。保留 `/anthropic` 后缀，因为 Codewhale 会追加 `/v1/messages`。该路由使用 `MINIMAX_API_KEY`，默认 `MiniMax-M3`；`MiniMax-M2.7` 也已注册。官方 M3 输入模态是文本、图像和视频，带自适应或禁用思考。M2.7 仅文本，总是保持思考启用。

- `api_key`(字符串，托管 provider 必填)：对 DeepSeek/托管 provider 必须非空(或设置 provider API key 环境变量)。自托管 SGLang、vLLM 和本地 `ollama` 可省略。`ollama-cloud` 需要为该 provider 保存的密钥或由 `OLLAMA_CLOUD_API_KEY` 提供，然后 `OLLAMA_API_KEY`。

- `auth_mode`(字符串，可选的 provider 表键)：选择 provider 特有的认证契约。Kimi Code 会员使用 `auth_mode = "api_key"`(或省略该字段)，在 [Kimi Code 控制台](https://www.kimi.com/code/console) 创建的 key，`base_url = "https://api.kimi.com/coding/v1"`，K3 用裸 `model = "k3"`。Codewhale 给该路由安全的 262,144-token 基线；只有 Kimi Code 计划包含 1M 访问(Allegretto 及以上)时才设置 `context_window = 1048576`。`k3[1m]` 是仅 Claude Code 的约定，不是 API 模型 ID,Codewhale 会拒绝它，而不是静默改变线模型或假定资格。`model = "kimi-for-coding"` 仍是所有 Kimi Code 会员可用的有效 K2.7 兼容路由。旧 `auth_mode = "kimi_oauth"` 以 API key 指引失败关闭，从不探测、读取、刷新或重写 `kimi_cli`/`kimi_code_cli` 凭据文件。一等 OAuth 需要 Codewhale 自己的厂商注册客户端身份，仍在 #4417 跟踪。

- `base_url`（字符串，可选，`[providers.<name>]` 键；较早的顶层写法见[旧版顶层 `base_url` 与 `api_key`](#旧版顶层-base_url-与-api_key)）：DeepSeek 的 OpenAI 兼容 Chat Completions API 默认 `https://api.deepseek.com/beta`,包括旧 `provider = "deepseek-cn"` 配置。其他默认：`deepseek-anthropic` 是 `https://api.deepseek.com/anthropic`,`nvidia-nim` 是 `https://integrate.api.nvidia.com/v1`,`openai` 是 `https://api.openai.com/v1`,`atlascloud` 是 `https://api.atlascloud.ai/v1`,`wanjie-ark` 是 `https://maas-openapi.wanjiedata.com/api/v1`,`volcengine` 是 `https://ark.cn-beijing.volces.com/api/coding/v3`,`openrouter` 是 `https://openrouter.ai/api/v1`,`xiaomi-mimo` 在 API key 以 `tp-...` 开头时是 `https://token-plan-sgp.xiaomimimo.com/v1`,否则 `https://api.xiaomimimo.com/v1`,`novita` 是 `https://api.novita.ai/openai/v1`,`fireworks` 是 `https://api.fireworks.ai/inference/v1`,`siliconflow` 是 `https://api.siliconflow.com/v1`,`siliconflow-CN` 是 `https://api.siliconflow.cn/v1`,`arcee` 是 `https://api.arcee.ai/api/v1`,`moonshot` 是 `https://api.moonshot.ai/v1`,`minimax` 是 `https://api.minimax.io/v1`,`openmodel` 是 `https://api.openmodel.ai`,`zai` 是 `https://api.z.ai/api/coding/paas/v4`,`stepfun` 是 `https://api.stepfun.ai/v1`,`deepinfra` 是 `https://api.deepinfra.com/v1/openai`,`sakana` 是 `https://api.sakana.ai/v1`,`huggingface` 是 `https://router.huggingface.co/v1`,`modelscope` 是 `https://api-inference.modelscope.cn/v1`,`together` 是 `https://api.together.xyz/v1`,`qianfan` 是 `https://api.baiduqianfan.ai/v1`,`openai-codex` 是 `https://api.openai.com/v1`,`anthropic` 是 `https://api.anthropic.com`,`mistral` 是 `https://api.mistral.ai/v1`,`sglang` 是 `http://localhost:30000/v1`,`vllm` 是 `http://localhost:8000/v1`,`ollama` 是 `http://localhost:11434/v1`,`ollama-cloud` 是 `https://ollama.com/v1`。中国区域小米 MiMo Token Plan 账号设置 `base_url = "https://token-plan-cn.xiaomimimo.com/v1"`，欧洲/阿姆斯特丹账号设置 `base_url = "https://token-plan-ams.xiaomimimo.com/v1"`。Mistral 特有的推理字段和多态重放只在记录的官方 HTTPS `/v1` 主机上启用；自定义 Mistral base URL 保持通用 Chat 语义。显式设置 `https://api.deepseek.com` 或 `https://api.deepseek.com/v1` 可选择退出 DeepSeek beta 功能。

- `ollama-cloud` 路由：选择 `provider = "ollama-cloud"`，覆盖默认 `https://ollama.com/v1` / `gpt-oss:120b` 元组时配置 `[providers.ollama_cloud]`，并用 `codewhale auth set --provider ollama-cloud` 从 [Ollama 账号设置](https://ollama.com/settings/keys) 保存 key。环境优先级是 `OLLAMA_CLOUD_API_KEY`，然后 `OLLAMA_API_KEY`；任意 Ollama 模型 ID 原样透传。
- 旧 Ollama Cloud 迁移：规范化的 `[providers.ollama].base_url` 恰好是 `https://ollama.com/v1` 的已发布 `provider = "ollama"` 配置，会在内存中升级为 `ollama-cloud` 运行时身份。只有那个确切元组可以读取其旧的 `ollama` provider 表和机密槽位。配置和机密从不被重写，相邻路径、HTTP 降级、仿冒主机或显式 `ollama-cloud` 选择从不消费该回退。

- `telecomjs` base URL 与目录：`[providers.telecomjs]` 默认 `https://aigw.telecomjs.com/v1`;`TELECOMJS_BASE_URL` 覆盖它。有 `TELECOMJS_API_KEY` 时，`/models` 刷新 key 作用域的目录，而不会把行混入另一个 provider。

- `edenai` 网关：选择 `provider = "edenai"`；`[providers.edenai]` 默认 `https://api.edenai.run/v3` 和 `deepseek/deepseek-v4-pro`。接受 `EDENAI_API_KEY`、`EDENAI_BASE_URL` 和 `EDENAI_MODEL`。Eden AI 记录的欧盟端点用 `EDENAI_BASE_URL = "https://api.eu.edenai.run/v3"`；默认 `deepseek/deepseek-v4-pro` 只列在全局目录中，所以把欧盟端点与欧盟列表中的模型(如通过 `EDENAI_MODEL` 或 `model` 的 `qwen/deepseek-v4-pro`)配对。该 provider 刷新 Eden AI 的 `/models` 目录，但保留模型特有的推理控制不动，因为网关横跨多个模型家族。

- `codewhale`（Codewhale API）：选择 `provider = "codewhale"`；`[providers.codewhale]` 默认 `https://api.codewhale.net/v1` 和 `deepseek/deepseek-v4-pro`。凭据是带有 `models:infer` 作用域的 Codewhale 账号 API key（`cwc_key_…`），读取自 `CODEWHALE_API_KEY` 或 `codewhale` 机密存储槽位；`codewhale account api-keys create --name <name> --use` 会创建一个并保存到本地。`CODEWHALE_API_BASE` 覆盖源地址，除 loopback 外必须是 HTTPS。模型目录是该账号自己经认证的 `GET /v1/models`：id 形如 `provider/model`，每一行都会声明它的线协议（`chat-completions` → `/v1/chat/completions`，`anthropic-messages` → `/v1/messages`，`responses` → `/v1/responses`）。用 `codewhale account keys set <provider>` 连接底层 provider 的 key。

- `concentrate` 网关：选择 `provider = "concentrate"`；`[providers.concentrate]` 默认 `https://api.concentrate.ai/v1` 和 `deepseek-v4-pro`，使用 OpenAI Responses 线协议。接受 `CONCENTRATE_API_KEY`、`CONCENTRATE_BASE_URL` 和 `CONCENTRATE_MODEL`。模型 id 原样透传（`gpt-5.6-sol`、`openai/gpt-5.6-sol`，或网关路由器用的 `concentrate/auto`）。仅支持 BYOK；见 [PROVIDERS.md](PROVIDERS.md)。

- `mistral` 模型与推理契约：`[providers.mistral]` 默认 `mistral-code-latest`；`MISTRAL_MODEL` 覆盖它，两者都设置时通用 `CODEWHALE_MODEL` 覆盖胜出。当前选择器还列出 `mistral-medium-latest`、`mistral-small-latest` 和 `mistral-large-latest`。在确切的官方 HTTPS `/v1` 路由上，Medium 和 Small 只接受 `reasoning_effort = "none" | "high"` 并重放多态思考块。弃用的原生 Magistral ID 仍可显式配置，保持始终推理，绝不接收可调的强度字段。

- `context_window`(整数，可选的 provider 表键)：当 OpenAI 兼容网关、托管模型别名或自托管运行时的上限与 Codewhale 的静态模型表不同时，覆盖活动 `[providers.<name>]` 路由的总上下文窗口。例如，`[providers.openai] context_window = 1000000` 让 OpenAI 兼容的 DashScope/Qwen 路由按 1M-token 窗口做预算，而不是保守回退。对 Kimi Code K3，保持 `model = "k3"`，只在会员计划包含 1M 访问时设置 `[providers.moonshot] context_window = 1048576`；否则省略它，以保留 262,144-token 安全基线。该值必须大于 0，影响提示上下文备注、压缩阈值、上下文压力检查和请求输出上限。完整解析顺序，以及如何看到哪一级产生了当前窗口：[上下文长度(context window)](#上下文长度context-window)。

- `path_suffix`(字符串，可选的 provider 表键)：覆盖不为 `/v1/chat/completions` 服务的 OpenAI 兼容网关的聊天补全路径。例如，`[providers.openai] path_suffix = "/chat/completions"` 把聊天请求发送到未版本化的 base URL 加 `/chat/completions`；`models` 和 `beta/*` 请求保持正常路由。

- `reasoning_stream_style`(字符串，可选的 provider 表键)：覆盖活动 provider 路由如何把流式推理与答案文本分开。用 `separate_field` 处理 `reasoning_content` / `reasoning` 增量，`inline_tags` 用于在 `delta.content` 内流式 `<think>...</think>` 的网关，`none` 则把传入内容完全按答案文本渲染。未设置时，每条 Chat Completions 路由都使用 `separate_field`(Mistral 一方路由使用其类型化思考块)；仅当网关把答案放在 `reasoning_content` 里流式发送时才设置 `none`。

- `[providers.<name>.auth]`(表，可选):provider 作用域的认证源元数据。`source = "command"` 存储命令 argv 加可选 `timeout_ms`；`source = "secret"` 存储 `secret_id`。这个切片让 provider 就绪、`/provider` 和 doctor JSON 报告认证源类别，而不暴露命令 argv 输出或机密值；执行命令和解析外部机密材料由后续的解析器工作处理。

- `insecure_skip_tls_verify`(bool，可选的 provider 表键)：旧兼容键，默认禁用。活动 provider 表上为 true 时，provider 客户端拒绝该配置，而不是跳过 TLS 证书验证。企业或私有 CA 包用 `SSL_CERT_FILE`；`codewhale doctor` 报告此设置的过期使用。

- `default_text_model`（字符串，可选）：DeepSeek 和 `deepseek-anthropic` 默认 `deepseek-flash`，OpenAI 是 `gpt-5.6`，xAI 是 `grok-4.6`，NVIDIA NIM 是 `deepseek-ai/deepseek-v4-pro`，AtlasCloud 是 `deepseek-ai/deepseek-v4-flash`，Wanjie Ark 是 `deepseek-reasoner`，火山方舟是 `DeepSeek-V4-Pro`，OpenRouter 和 Novita 是 `deepseek/deepseek-v4-pro`，小米 MiMo 是 `mimo-v2.6-pro`，Fireworks 是 `accounts/fireworks/models/deepseek-v4-pro`，SiliconFlow 和 DeepInfra 是 `deepseek-ai/DeepSeek-V4-Pro`，Arcee AI 是 `trinity-large-thinking`，Moonshot 是 `kimi-k2.7-code`，MiniMax 是 `MiniMax-M3`，Z.ai 是 `GLM-5.3`，StepFun 是 `step-3.7-flash`，千帆是 `ernie-4.0-turbo-8k`，Sakana AI 是 `fugu`，SGLang/vLLM 是 `deepseek-ai/DeepSeek-V4-Pro`，本地 Ollama 没有固定 id（Codewhale 会从实时本地目录中选用一个支持聊天的标签——优先选 coder 或支持工具的标签，然后是上下文最大的——绝不会选嵌入模型或重排序模型），Ollama Cloud 是 `gpt-oss:120b`。Hugging Face 和 Together AI 都默认 `deepseek-ai/DeepSeek-V4-Pro`；`openai-codex` 默认 `gpt-6.1-sol`；`anthropic` 默认 `claude-sonnet-4-6`；`openmodel` 默认 `deepseek-v4-flash`。当前公开的 DeepSeek ID 包括 `deepseek-v4-pro` 和 `deepseek-flash`（V4.1 Flash，以不带版本号的 id 发布），两者都是 1M 上下文窗口、384K 最大输出、默认启用思考模式。DeepSeek 的实时定价/模型页现在把 Pro 后端标为 `DeepSeek-V4-Pro-0813`；可调用的 API ID 仍是 `deepseek-v4-pro`，所以 Codewhale 不发送后端标签，也不发送 Claude Code 特有的 `deepseek-v4-pro[1m]` 选择器。DeepSeek 已于 2026 年 7 月 24 日退役 `deepseek-chat` 和 `deepseek-reasoner`；直连官方路由把两者都迁移到 `deepseek-flash`，省略推理设置时保留它们此前的不思考（`off`）和思考（`high`）意图。DeepSeek 也已退役 `deepseek-v4-flash` 和 `deepseek-v4-flash-vision-exp` 模型：两个名称仍被接受，由 V4.1 Flash 提供服务，并按 `deepseek-flash` 的价格计费；Codewhale 对 2026-09-10 04:00 UTC 起的回合采用该价格。`flash`、`deepseek-v4flash`、`flash-vision` 和 `deepseek-v4flashvisionexp` 简写选择 `deepseek-flash`。显式的 `reasoning_effort` 优先，Wanjie Ark、聚合器、自托管运行时和自定义端点上由 provider 自己拥有的 id 不会被全局改写。SiliconFlow 保留自己的映射：`deepseek-reasoner` 和 `deepseek-r1` 选择其 Pro 模型，而 `deepseek-chat` 和 `deepseek-v3` 选择 Flash。各 provider 特有的映射在受支持之处把 `deepseek-v4-pro` / `deepseek-v4-flash` 翻译成该 provider 的模型 ID。OpenRouter 还识别最近的大模型 ID，如 `arcee-ai/trinity-large-thinking`、`minimax/minimax-m3`、`minimax/minimax-m2.7`、`xiaomi/mimo-v2.5-pro`、`qwen/qwen3.6-flash`、`qwen/qwen3.6-35b-a3b`、`qwen/qwen3.6-max-preview`、`qwen/qwen3.6-27b`、`qwen/qwen3.6-plus`、`qwen/qwen3.7-max`、`google/gemma-4-31b-it`、`moonshotai/kimi-k2.7-code`、`moonshotai/kimi-k2.6`、`nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free` 和 `nvidia/nemotron-3-ultra-550b-a55b`；直连 Arcee 使用 `trinity-large-thinking` 和 `trinity-large-preview` 这样的裸 ID；直连 Moonshot 识别 `kimi-k3`、`kimi-k2.7-code` 和 `kimi-k2.6`。确切的 Kimi Code 端点识别 K3 的裸 `k3` 和 K2.7 的 `kimi-for-coding`；这些会员 ID 与直连 Moonshot 的 ID 不同，绝不会跨路由改写。直连 MiniMax 识别 `MiniMax-M3` 和有文档记录的 M2.x 聊天模型 ID；直连 Z.ai 识别 `GLM-5.3`（默认）、`GLM-5.2`、`GLM-5.1` 和 `GLM-5-Turbo`，OpenRouter 识别对应的 `z-ai/glm-5.1`、`z-ai/glm-5.2`、`z-ai/glm-5.3` 和 `z-ai/glm-5-turbo` ID——`GLM-5.3` 自 2026-08-13 起已在 Z.ai Coding Plan 上线；在 Z.ai 发布独立的 5.3 数据之前，它继承 `GLM-5.2` 的目录元数据且不带价格，而显式选择 `GLM-5.2` 会保留它自己的 id；直连 Sakana 识别 `fugu` 和 `fugu-ultra-20260615`；直连小米 MiMo 识别聊天 ID `mimo-v2.6-pro`、`mimo-v2.6-flash` 和 `mimo-v2.5-pro-ultraspeed`（已停用的 `mimo-v2.5-pro` 和 `mimo-v2.5` 会以 `mimo-v2.6-pro` 和 `mimo-v2.6-flash` 发送），而 TTS ID 通过 `codewhale speech` / `tts` 选择。通用 `openai`、`atlascloud`、`wanjie-ark`、`xiaomi-mimo`、`arcee`、`moonshot`、`minimax`、`openmodel`、`zai`、`stepfun`、`qianfan`、`sakana`、本地 Ollama 和 Ollama Cloud 的模型 ID，在已知别名规范化之后原样透传。带自定义 `base_url` 的 OpenRouter 和 SiliconFlow provider 配置也保留显式的模型值，这样 OpenAI 兼容网关就能接受裸模型 ID。用 `/models` 或 `codewhale models` 从你配置的端点发现实时 ID。`CODEWHALE_MODEL` 为单个进程覆盖它；`DEEPSEEK_MODEL` 是旧别名。
- TelecomJS 只把 `deepseek-v4-pro` 用作刷新前的保守回退。其 key 作用域的 `/models` 目录可用后，选择器使用那些实时行；Codewhale 在该路由上省略不受支持的推理请求字段。

- `reasoning_effort`（字符串，可选）：`auto`、`off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`ultra`（`ultracode` 是 `ultra` 的别名）或 `max`；默认取已配置的 UI 档位。DeepSeek Platform 收到顶层 `thinking` / `reasoning_effort` 字段。Ollama Cloud 的 OpenAI 兼容 Chat Completions 路由保留其有文档记录的 `none` / `low` / `medium` / `high` / `max` 阶梯（`off` 作为 `none` 发送；`xhigh` 和 `ultracode` 规范化为 `max`）。确切 `https://api.x.ai/v1` 上的直连 xAI `grok-4.7` 和 `grok-4.6` 收到顶层 `reasoning_effort = "low" | "medium" | "high" | "xhigh"`（阶梯来自内置目录行；`grok-4.5` 把 `xhigh` 映射为 `high`，没有文档记录 effort 的行，如 `grok-4.3`，则不发送该字段）；Grok 的推理无法关闭，因此 `off` 规范化为 `high`，`max`/`ultracode` 规范化为 `xhigh`，`auto` 让该字段省略，从而使用 xAI 文档记录的默认值 `high`。自定义的 xAI 兼容 `base_url` 不继承该方言。确切 `https://api.moonshot.ai/v1` 上的直连 Moonshot `kimi-k3` 始终思考，只收到顶层 `reasoning_effort = "low" | "high" | "max"`；`off` 规范化为 `low`，`medium` 规范化为 `high`。确切 `https://api.kimi.com/coding/v1` 上的 Kimi Code 会员 `k3` 则收到嵌套的 `thinking.effort`，其 `off` 设置同样规范化为启用的 `low`。正常调度的 `auto` 使用 Codewhale 的自动推理选择器，发送一个具体的、按路由规范化后的档位；只有省略推理设置时，才由 provider 的默认值控制。相邻的网关以及其他模型/端点组合保留通用的 Moonshot 契约。OpenAI Codex 把过期的 `off` 规范化为 `low`，并把 `max` / `ultracode` 作为 Responses 的 `xhigh` 发送。Z.ai 收到有文档记录的 `thinking` 控制，并把启用的思考视为 GLM coding 的 high/max 通道。NVIDIA NIM 通过 `chat_template_kwargs` 收到等效设置。

- `verbosity`(字符串，可选)：`normal` 或 `concise`。`normal` 保持默认的对话式提示。`concise` 追加一块提示纪律，用于直接、少废话的输出；CLI 非交互命令(`exec` 和 `eval`)默认 `concise`，除非 config/环境/CLI 覆盖它。用 `CODEWHALE_VERBOSITY` 或旧别名 `DEEPSEEK_VERBOSITY` 按进程覆盖。

- `telemetry`（bool，可选）：匿名使用计数，**在当前 0.9.12 源码中默认 `true`**。第 `5` 版告知列明 Codewhale 和 PostHog，并说明退出策略；不会替默认用户虚构一次同意。此前已明确退出的选择继续保持关闭。这里的显式 `false` 是持久的*退出*：它会删除随机的安装 id、清空缓冲的和 dry-run 的事件，并留下一个只要该设置为 false 就会被重新声明的 tombstone。它是底线：`--telemetry true` 和 `CODEWHALE_TELEMETRY=1` 都会输给它。用 `/settings` 或 `codewhale config set telemetry true`，可以经由现有的隐私转换流程为新会话显式重新开启计数。`CODEWHALE_TELEMETRY`（旧别名 `DEEPSEEK_TELEMETRY`）和 `--telemetry false` 提供作用于单次运行的关闭开关，停止采集和发送，但不擦除所有者的状态。仓库本地的 `.codewhale/config.toml` 不能设置这个偏好。`codewhale config telemetry` 显示告知，`codewhale config get telemetry` 报告偏好和隐私状态。完整 schema 和退出行为：[`TELEMETRY.md`](TELEMETRY.md)。

- `telemetry_endpoint`(字符串，可选)：批次 POST 到哪。保持未设置会选择随附默认 **`https://telemetry.codewhale.net/v1/telemetry`**——[`TELEMETRY.md`](TELEMETRY.md) 中描述的第一方接入服务，其源码在 `telemetry-ingest/`。这个键只决定*被允许的会话*发送到哪里；它不能覆盖选择退出。把它设为**空字符串**是保持启用且不联系任何人的方式：每个批次随后写入 `$CODEWHALE_HOME/telemetry/dryrun.jsonl`，完全不构造 HTTP 客户端，所以你可以读到原本会发送的确切内容。任何其他值直接替换默认值。要求 `https://`;普通 `http://` 只对 loopback 主机接受，并且没有环境变量可以覆盖该拒绝。被拒绝的端点会让遥测在本次运行关闭，而不是回退到明文或默认值。用 `CODEWHALE_TELEMETRY_ENDPOINT`(旧别名 `DEEPSEEK_TELEMETRY_ENDPOINT`)按进程覆盖，其中空值意味着同样的"不联系任何人"。仓库本地的 `.codewhale/config.toml` 不能设置它。

- `allow_shell`(bool，可选)：在交互式 TUI Agent 会话中，省略它会保持 shell 工具可用但带审批提示；设为 `false` 会隐藏 shell 工具。无头、持久任务和其他非交互 profiles 保持保守的省略字段默认，需要 `allow_shell = true` 才暴露 shell。Plan 模式总是隐藏 shell;Full Access 启用 shell 和自动批准。

- `approval_policy`(字符串，可选)：`on-request`、`untrusted` 或 `never`。`/config` 中的运行时 `approval_mode` 编辑也接受 `on-request` 和 `untrusted` 别名。

- `[approval] default_selection`(字符串，可选)：审批卡片首次出现时高亮哪个选项——`deny`(默认)或 `allow_once`。`deny` 意味着在没读过的卡片上反射性按 Enter 会拒绝调用。高亮的那一行会标注 `(Enter)`；按 `y` 可仅允许本次。设置 `allow_once` 恢复 v0.9.6 之前的 Enter 即批准肌肉记忆(#5293)。它只移动高亮：哪些调用会被提示仍由 `approval_policy` 加 `permissions.toml` 中的规则决定。

  ```toml
  [approval]
  default_selection = "allow_once"
  ```

- `[approval] timeout_seconds`（整数，可选）：限定一次审批最多可以等待多久——TUI 卡片，以及桌面应用和 web 使用的 Runtime API 审批同样适用。窗口过期后，该调用会被拒绝（fail closed），并记录为超时，而不是记录为操作者的拒绝。省略或为 `0` 表示无限期等待，这在各处都是默认值：除非你设置它，否则任何审批都不会被替你拒绝。超过 24h 的值会带警告被钳制（#6101）。

  ```toml
  [approval]
  timeout_seconds = 300
  ```

- `sandbox_mode`（字符串，可选）：`read-only`、`workspace-write`、`danger-full-access`、`external-sandbox`。配置值只能收窄权限模式选定的沙箱（Full Access 不套包装器；Ask 和 Auto-Review 使用 `workspace-write`；Plan 使用 `read-only`），因此 `danger-full-access` 和 `external-sandbox` 只在 Full Access（`yolo`）下生效。各平台的支持并不相同。macOS 在其运行时探测成功时使用 Seatbelt。Linux 默认在 `/usr/bin/bwrap` 已安装且探测证明它能限制子进程时使用 bubblewrap；`prefer_bwrap = false` 退出并报告没有 OS 命令沙箱。Windows 目前不宣称有 OS 沙箱；其规划中的辅助程序契约只从进程树隔离开始，在只读文件系统隔离、workspace-write 强制、网络阻断、注册表隔离或 AppContainer 隔离真正实现之前，不得被描述成这些能力。
- 模式准入、hooks、已注册工具的要求、类型化规则、Auto-Review、仓库保护规则、人工审批和执行沙箱之间的跨层关系，定义在[授权顺序](../AUTHORIZATION_ORDER.md)中。
- **读取拒绝列表。** 每一种沙箱档位——包括 `read-only`——都授予对整个文件系统的读取权限；这些档位的区别在于它们可以*写入*什么，以及能否访问网络。读取拒绝列表在两个范围内收窄这一点。Codewhale 自己的文件工具（`read`、`grep`、`file_search`、`read_media`）会拒绝列表中的每一条路径。shell 命令只拒绝其中的*密钥材料和其他主体的*条目——SSH 与 GPG 密钥、`/etc/shadow`、`/etc/sudoers`、系统与登录钥匙串、浏览器配置文件、其他 agent 的凭据存储、Codewhale 自己的机密存储、运行时守护进程套接字，以及 `sandbox_denied_read_paths` 中的任何路径。工具自身的认证配置对模型拒绝，但对工具本身保持可读：`gh` 没有 `~/.config/gh/hosts.yml` 就无法启动，因此对进程拒绝该文件只会让 CLI 无法运行，却并不会让它的 token 更难得手。
  - `sandbox_read_denylist_defaults`（bool，默认 `true`）：应用内置的凭据存储集合——`~/.ssh`、`~/.gnupg`、云凭据目录（`~/.aws`、`~/.config/gcloud`、`~/.azure`、`~/.kube` 等）、`~/.netrc`、`~/.npmrc`、`~/.git-credentials`、macOS 钥匙串、浏览器配置文件、Codewhale 自己的机密存储，以及 `.env` 文件（但不包括 `.env.example` 之类）。普通源码、`Cargo.toml`、`~/.gitconfig`、`~/.cargo` 和 `~/.npm` 保持可读，因此构建和测试仍能工作。设为 `false` 可恢复 0.9.12 之前的整盘读取行为。
  - `sandbox_denied_read_paths`（路径列表）：额外被拒绝的子路径。`~` 会展开。这些永远不能被豁免。
  - `sandbox_read_denylist_exempt`（路径列表）：当项目确实需要时，从*内置默认值*中减去一个路径。拒绝优先于允许：这永远不会重新打开 `sandbox_denied_read_paths` 中的任何内容。

    豁免的粒度是**整条规则**，而不是逐个文件。只有当内置规则自身的路径位于豁免路径之上或等于它时，豁免路径才会移除该规则，所以豁免 `~/.ssh/config` 什么也不会改变：`~/.ssh` 这条规则仍然拒绝它，因为 `~/.ssh` 并不位于 `~/.ssh/config` 之内。要重新打开这一个文件，你必须豁免 `~/.ssh` 本身——这也会重新打开它旁边的私钥。这是文档中明确记录的权衡：没有提供把内置规则收窄为“除某个文件之外的一切”的办法；请把你需要的内容复制出被拒绝的目录树。唯一一条按名称匹配的规则，即 `.env` 文件，是按*名称*而不是按路径豁免的：任何文件名恰好为 `.env` 的豁免条目——裸 `.env`、`~/.env`、`some/project/.env`——都会禁用整条 `.env` 文件名规则，也就是磁盘上的每一个 `.env` 和 `.env.<name>`，而不只是某一个项目的。（`.env.example` 之类从不被拒绝，因此无需豁免。）

  在两个位置强制执行，但范围不同。Codewhale 自己的进程内工具（OS 沙箱从不包裹它们）会拒绝每一条规则——用于读取内容的 `read_file` / `read` / `read_media`，以及用于*枚举*的 `list_dir` / `file_search`（列出被拒绝的目录或在其中搜索都会被拒绝，正如 Seatbelt 阻止其 readdir 一样；根在被拒绝目录树之上的名称搜索会跳过其中的条目）。沙箱内的 shell 命令（Seatbelt 采用最后匹配者胜出的 `deny file-read*` 规则；bubblewrap 屏蔽每个路径）只拒绝其中的密钥材料和其他主体规则，因此已认证的 CLI 仍能读取自己的配置。被拒绝的读取始终是显式错误，绝不是空结果，并且错误里写出的是调用方所拼写的路径，而不是符号链接目标的真实位置。

  **这是纵深防御，不是安全边界。** 它无法阻止指向被拒绝文件的硬链接、已经复制进工作区的机密、间接读取（`ssh-agent`、`security find-generic-password`、`aws sts …`）、`danger-full-access` 下 shell 命令的读取、MCP 服务器或其他未被包裹的子进程的读取，也无法阻止对*已经被*读取的内容的外泄。真正起作用的仍应是最小权限的凭据和短期令牌。

- `permissions.toml`(同级文件，可选)：与 `config.toml` 相邻加载的类型化权限规则记录，例如 `~/.codewhale/permissions.toml`。这个活动用户文件是今天唯一的权限规则来源；项目配置覆盖不加载项目本地的 `permissions.toml`。规则的 `workspace` 可选字段是它的仓库作用域，不是第二个来源。手工编写的 `[[rules]]` 条目接受 `tool`、可选 `command` 或 `path`、可选绝对 `workspace`、可选 `command_exact = true` 和可选 `action = "deny" | "ask" | "allow"`；省略 `action` 默认 `"ask"`。`workspace` 把规则限制到该仓库，而 `command_exact = true` 把命令规则从历史的 arity 感知前缀匹配改为完整命令匹配。`deny` 在基于模式的审批处理之前阻止匹配调用，`allow` 跳过匹配调用的审批，`ask` 只在可以提示的模式中强制审批。在 TUI 自动批准路径之外，`approval_policy = "never"` 下匹配的 `ask` 规则被拒绝，因为没有提示可显示。在 Full Access / 自动批准会话中，`ask` 规则不会将会话降级为提示或阻断；显式 `deny` 规则仍按当前执行策略逻辑阻断。

  在受支持的审批卡片中，按 `S` 允许该请求一次，并把精确的 `action = "ask"` 规则追加到这个文件。对于符合条件的安全请求，选择**始终允许本仓库中的这条确切规则**(快捷键 `P`)来追加带当前绝对 `workspace` 作用域的 `action = "allow"` 规则。记住的 shell 授予设置 `command_exact = true`，所以之后带额外参数的命令不会继承该授予。文件和补丁授予保留现有验证路径产生的精确工作区相对路径。受支持的保存刻意很窄：`exec_shell` 存储确切批准的命令字符串；`write_file` 和 `edit_file` 存储确切工作区相对文件路径；`apply_patch` 为 apply-patch 预检验证过的每个触碰文件存储一条确切的工作区相对 `path` 规则。现有 exec 命令匹配对手工编写的前缀规则保持 arity 感知；审批卡片允许授予使用完整命令匹配。文件路径规范化为运行时匹配使用的同一工作区相对形式。

  `read_file` 规则仍可手工编写，当你希望未来对特定路径的读取询问、允许或拒绝时，但审批 UI 不保存 `read_file` 规则。被分类为需要审批或危险的命令、关键审批卡片和仓库保护规则提示不能保存允许授予，继续需要审查。

  `/permissions`(或 `/permissions list`)是窄规则管理面。它列出每个编号规则，带活动用户文件来源、确切有效匹配器(工具级、命令前缀、确切命令或确切规范化路径)、全局或仓库作用域，以及该作用域是否在当前工作区应用。`/config ask-rules` 仍是同一列表的兼容入口。

  删除受审查门控：`/permissions remove <number>` 只预览所选规则并打印一条确认命令。该命令携带绑定到确切文件字节和规则索引的不透明 token；如果另一个写入者改变了 `permissions.toml`，确认会失败，而不是删除移入旧位置的规则。确认删除和审批卡片追加共享相邻的 `permissions.toml.lock`，保留不相关的 TOML 注释和格式，并原子替换文件。运行中的 TUI 重新加载用户规则集，不清除会话仅有的批准。

  这个编辑器刻意不创建或重写规则、不持久化审批卡片的拒绝选择、不展开 glob、不创建宽泛的目录/递归规则。需要时手工编写那些受支持的确切/前缀记录。

- `[[hotbar]]`（表数组，可选）：TUI 热栏的用户自有 1-8 槽位绑定。每个条目有 `slot`、`action` 和可选的 `label`。热栏默认隐藏（#3807）：省略 `hotbar` 和设置 `hotbar = []` 都表示没有热栏。`/hotbar on` 会把八个默认槽位（`slash.workflow`、`slash.goal`、`slash.auto`、`mode.plan`、`mode.agent`、`mode.operate`、`palette.open`、`sidebar.toggle`）写成显式的 `[[hotbar]]` 表。存在一个或多个 `[[hotbar]]` 表时，该列表就是整个热栏；缺失的槽位保持为空。`1..=8` 之外的无效槽位会带警告被跳过，重复的槽位采用后面的条目，未知的动作 ID 会被保留，这样 UI 能显示禁用/未知的单元格，而不是悄悄删除用户配置。受信任的用户配置、profiles 和受管配置会替换整个列表；项目覆盖不能改变热栏绑定。持久化热栏绑定的 Setup 或向导流程，会把同样的 schema 写入解析出的 `~/.codewhale/config.toml` 路径，只有当旧的 `~/.deepseek/config.toml` 回退文件已经是活动配置时才保留它。

  ```toml
  [[hotbar]]
  slot = 1
  action = "mode.plan"
  label = "Plan"

  [[hotbar]]
  slot = 2
  action = "session.compact"
  ```

- `[auto_review]`(表，可选)：工具调用审查策略——确定性底线加模型守卫层。这一层位于现有权限档位之上；它可以拦住或阻断工具调用，但不是自动推送、自动合并或托管审查服务。先检查阻断规则，然后是内置安全底线，再是允许规则。在 Ask 中，安全拦阻打开审批，会话授权或 allow 规则都无法应答它；在 Auto-Review 或非交互 `never` 档位中，它作为硬阻断失败关闭。Full Access 跳过内置底线。即使 allow 规则匹配，安全底线仍覆盖类似发布的动作、破坏性的后台/无头动作，以及请求体携带凭据文件或名称表明是机密的变量、或读取由这两者或 `env`/`printenv` 送入的 stdin 的 `curl`/`wget` 上传；超过 64 KiB 或 4,096 个词、且文本中任意位置含有 `curl` 或 `wget` 的 shell 命令、shell 输入或校验器，以及嵌套过深而无法读取的调用，同样会被拦住。在 Full Access 之外，它还会在每个档位拒绝含有隐藏 Unicode 字符（双向控制符、零宽及其他不可见字符、标签字符、孤立的变体选择符，以及 shell 输入中 ASCII 控制键之外的终端控制字符）的 shell 命令、shell 输入或校验器，并写出该码位及其位置。

  ```toml
  [[auto_review.allow]]
  id = "read-only-inspection"
  action_kind = "read"
  reason = "Read-only inspection is safe to run automatically."

  [[auto_review.block]]
  id = "no-release-publish"
  action_kind = "publish"
  reason = "Release and publish actions require maintainer review."
  ```

  规则匹配器是精确 `tool` 和/或 `action_kind`。至少需要一个匹配器。`action_kind` 接受六个决策相关种类 `read`、`write`、`shell`、`external`、`publish` 和 `destructive`。无效名称让配置验证失败，而不是静默扩大到另一个策略类。在阻断规则中，旧名称保持保守兼容别名：`network`、`git`、`mcp_action`、`browser` 和 `unknown` 映射到 `external`；`secret` 映射到 `destructive`；`mcp_read` 映射到 `read`。允许规则中退役的窄种类验证失败，而不是扩大到更宽类别。退役的 `text_contains` 匹配器同样验证失败，而不是静默扩大旧的意图依赖规则。交互式 Auto-Review 中的回退拦阻升级为一次无状态守卫请求。请求包含确切被拦调用和确定性观察作为独立 JSON 字段。对话历史、技能指令、附加文件内容和其它展开的模型上下文被排除。守卫不推断用户意图，不算授权分数。它不暴露工具，返回风险级别、允许/拒绝和理由。高或关键风险即使模型说允许也不能自动运行。过大的确切调用被拒绝而不是截断。只做一次审查请求；不完整或格式错误的输出、超时、取消、provider 失败或空理由都失败关闭。确定性底线从不被模型审查，无头适配器使用仅确定性层级。固定的 Codex、Kimi 和 DeepSeek 源边界从[权限档位](MODES.md#权限姿态)链接。审查结果发出 `tool.auto_review` 审计事件，`gate = "guardian"`。

  Auto-Review 决策在启用工具审计日志时发出 `tool.auto_review` 审计事件，`gate = "deterministic"`。未来的 PreToolUse/PostToolUse hooks 可以在这层周围添加观察者输入，但配置的 Auto-Review 策略在工具调用被允许继续之前评估。

- `managed_config_path`(字符串，可选)：用户/环境配置之后加载的受管配置文件。

- `requirements_path`(字符串，可选)：用于强制允许的审批/沙箱值的需求文件。

- `max_subagents`(int，可选)：默认 `64`，钳制到 `1..=128`。

- `subagents.*`（可选的兼容表）：面向直接启动和 Workflow `agent` 启动的手动按角色模型固定项。显式保存的配置优先，然后是手动角色固定项，然后是唯一的已保存角色固定项。相互冲突的工具 `model` 或 `model_strength` 选择会在准入之前被拒绝。未固定的角色允许在继承默认值之前使用任务级 model/strength 选择。`[subagents.roles.<role>] model = "provider/model"` 折叠进现有的覆盖映射，并优先于 `[subagents.models]`，然后才是便捷键。结构化的规范角色键优先于旧别名。只有这种结构化写法才会把显式 provider 与模型后缀分开；未知 provider 会在准入之前失败。裸的结构化模型 id 继承活动 provider。旧的标量/映射值会原样保留带命名空间的 provider 自有 id。支持的便捷键是 `default_model`、`worker_model`、`scout_model`、`planner_model`、`reviewer_model`、`custom_model`、`max_concurrent`、`max_admitted`、`launch_concurrency`、`token_budget`、`api_timeout_secs` 和 `heartbeat_timeout_secs`。v0.9.x 键 `explorer_model`、`awaiter_model` 和 `review_model` 仍作为别名接受。`[subagents] max_concurrent` 值覆盖顶层 `max_subagents`，也钳制到 `1..=128`。`[subagents] max_admitted`(别名：`max_total`、`admission_limit`)是排队加运行子智能体的有界总数；默认 `1024`(`MAX_SUBAGENT_ADMISSION`，`crates/tui/src/config/subagent_limits.rs:21`，在 `config.rs:6400` 应用)，所以高扇出回合可以排队并排空，同时运行时启动压力保持有界，并钳制到 `max_concurrent..=1024`。`[subagents] launch_concurrency` 设置一次直接启动多少子智能体，其余排队等启动槽位；默认解析出的 `max_subagents` 上限，钳制到 `1..=max_subagents`(弃用的 `interactive_max_launch` 键作为别名接受，两者都设置时新键胜出)。`[subagents] token_budget` 是每个根 `agent` 运行及其后代的可选聚合 token 上限；未设置或 `0` 保留无限制的旧行为。`[subagents] api_timeout_secs` 控制子智能体模型调用的每步 API 超时，钳制到 `1..=3600`，`0` 或未设置保留 600 秒默认；超时尝试用指数退避重试(最多 5 次)，然后步骤以保留检查点中断。`[subagents] heartbeat_timeout_secs` 控制过期运行智能体清理，默认 `300`，钳制到 `30..=3600`，同时保持在解析出的 API 超时之上。`[subagents.providers.<provider>]` 接受同样的扇出、深度、预算和超时旋钮(`enabled`、`max_concurrent`、`max_admitted`、`launch_concurrency`、`max_depth`、`token_budget`、`api_timeout_secs`、`heartbeat_timeout_secs`)，并为省略的键继承全局 `[subagents]` 值。Provider 键接受 `deepseek`、`zai`、`openrouter`、`anthropic` 这样的规范名，加 `glm`(Z.ai)和 `deepseek_api`(直接 DeepSeek)这样的便捷别名：

  ```toml
  [subagents]
  max_concurrent = 20
  launch_concurrency = 20
  max_admitted = 200
  max_depth = 6

  [subagents.providers.deepseek]
  max_concurrent = 20
  launch_concurrency = 20
  max_admitted = 200

  [subagents.providers.glm]
  max_concurrent = 4
  launch_concurrency = 3
  max_admitted = 12
  max_depth = 2

  [subagents.providers.openrouter]
  max_concurrent = 5
  launch_concurrency = 3
  max_admitted = 20
  ```

  `/config subagents status` 打印全局值和活动 provider 的解析 profile，这样速率限制调整在 TUI 中可见。`[subagents.models]` 接受小写的 Fleet 角色键，如 `worker`、`scout`、`planner`、`reviewer`、`builder` 和 `verifier`；旧类型键在 v0.9.x 期间仍被接受。值在派生时对照活动 provider 验证；直接 DeepSeek 需要 DeepSeek ID，而 OpenAI 兼容/自定义 provider 路由把显式模型 ID 透传给该 provider。要把子智能体路由到不同于父会话的 provider，保存带显式 `provider` 和 `model` 字段的 Fleet/AgentProfile(包括 `lm-studio` 这样的用户命名自定义 provider)，并调用 `agent(profile: "...")`；见 [SUBAGENTS.md](SUBAGENTS.md)。

- `skills_dir`(字符串，可选)：默认 `~/.codewhale/skills`(每个技能是包含 `SKILL.md` 的目录)。工作区获得信任后，项目技能优先于全局技能，其中 `.codewhale/skills` 优先于 `.agents/skills` 等兼容目录。顶层 `./skills` 需要显式设置 `[skills] flat_workspace_root = true` 或 `skills_dir` 才会加载。运行时还发现全局 agentskills.io 兼容的 `~/.agents/skills` 和更广的 Claude 生态系统 `~/.claude/skills`。在默认捆绑策略且未设置 `skills_dir` 时，首次启动为常见工作流安装带版本的捆绑技能，包括技能创建、委派、MCP/插件脚手架、文档、演示文稿、电子表格及 PDF。飞书/Lark 不属于默认安装包。只有 CodeWhale 自有的根(`<workspace>/.codewhale/skills` 和 `~/.codewhale/skills`)是可写安装/导入目标；兼容 harness 根保持只读。裸 `/skills` 打开 Extensions → Skills(仅自有，零网络)；`/skills manage` 打开技能管理器。管理器、审计状态、来源标记和变更规则见 [SKILLS.md](SKILLS.md)，可移植 `SKILL.md` 包与 Claude Code 插件运行时之间的受支持边界见 [CLAUDE_PLUGIN_COMPAT.md](../CLAUDE_PLUGIN_COMPAT.md)。

- `[skills.bundled].install`（默认 `"auto"`，另可设 `"all"` 或 `"none"`）：独立控制捆绑安装。`auto` 保留删除即退出规则，设置 `skills_dir` 时停止安装；`all` 补装已启用的缺失技能，只写入 `$CODEWHALE_HOME/skills`（通常为 `~/.codewhale/skills`），不受发现目录覆盖影响；`none` 不写文件或版本标记。修改版和托管正文始终保留。
- `[skills.bundled].selected` / `disabled` / `explicit_only`（名称数组，默认 `[]`）：空选择列表启用整个安装包，非空列表限制安装和发现。禁用优先，并允许低优先级目录的同名副本生效。显式调用保留用户调用，移除模型索引及模型加载权限。别名沿用规范名称策略；仅影响全局自有目录中可识别的发行副本，不删除文件。配置档逐字段合并，显式空列表清空继承值。`doctor` 报告未知及忽略条目，`/skills inspect` 显示路径和原因。详见 [SKILLS.md](SKILLS.md#捆绑技能)。
- `[skills].listing_max_description_chars`（整数，默认 `400`，范围 `1..=40000`）：父代理、子代理提示及 `/skills inspect` 共用的索引描述上限。
- `[skills].listing_budget_fraction`（有限数值，默认 `0.05`，大于 `0` 且不大于 `1`）：同一索引占路由窗口的比例，按每 token 四个字符估算，并限制在 `2400..=40000` 字符。配置档中的字段分别覆盖继承值；`/context` 使用相同解析。完整技能加载不受影响。
- `[skills].scan_codewhale_only`(bool，默认 `false`)：为 `true` 时，会话技能发现忽略跨工具根，如 `.claude/skills`、`.opencode/skills`、`.cursor/skills` 和 `~/.agents/skills`。Codewhale 仍扫描 `<workspace>/.codewhale/skills`、`~/.codewhale/skills` 和任何显式 `skills_dir` 覆盖。技能管理器仍可独立于这个运行时旋钮切换本地兼容审计扫描——见 [SKILLS.md](SKILLS.md)。

- `[skills].registry_url` / `[skills].max_install_size_bytes`(可选)：`/skills --remote`、`/skills suggest <task>`、`/skills sync` 和 `/skill install|update` 使用。默认管理器打开路径不联系 registry。

- `[verifier].enabled`(bool，默认 `false`)：该运行时触发点激活后启用自动的"已完成"声明验证器预览。`false` 时手动 `run_verifiers` 工具仍可用。

- `mcp_config_path`(字符串，可选)：默认 `~/.codewhale/mcp.json`，Codewhale 路径缺失时旧 `~/.deepseek/mcp.json` 回退。自定义路径必须绝对；相对值回退到用户全局路径，这样改变启动目录不能静默改变 MCP 池。它在 `/config` 中可见，可从 TUI 更改。新路径被 `/mcp` 立即使用，但重建模型可见的 MCP 工具池需要重启 TUI。

- `notes_path`(字符串，可选)：默认 `~/.codewhale/notes.txt`，Codewhale 路径缺失时旧 `~/.deepseek/notes.txt` 回退，由模型可见的 `note` 工具使用。

- `[memory].enabled`(bool，可选)：默认 `false`。为 `true` 时，TUI 把用户记忆文件加载进 `<user_memory>` 提示块，在输入区启用 `# foo` 快速捕获，浮现 `/memory` 斜杠命令，并注册 `remember` 工具。同一开关可通过 `DEEPSEEK_MEMORY=on` 使用。

- `memory_path`(字符串，可选)：锚定原生记忆存储。配置的文件名**不是**被写入的文件。在 Native 后端(唯一后端)下，存储被重新根到 `<parent-of-memory_path>/memory/global/MEMORY.md`——所以默认 `~/.codewhale/memory.md` 产生 `~/.codewhale/memory/global/MEMORY.md`(加工作区作用域文件和可重建的 SQLite FTS5 索引)。完整功能面见 [`MEMORY.md`](./MEMORY.md)(`# foo` 输入区前缀、`/memory` 斜杠命令、`remember` 工具、选择加入开关)。

- `snapshots.*`(可选)：用于文件回滚的 side-git 工作区快照：
  - `[snapshots].enabled`(bool，默认 `true`)
  - `[snapshots].max_age_days`(int，默认 `7`)
  - 快照位于 `~/.codewhale/snapshots/<project_hash>/<worktree_hash>/.git`，旧 `~/.deepseek/snapshots/...` 仅在旧状态存在时回退，从不使用工作区自己的 `.git` 目录

- `context.*`(可选):
  - `[context].project_pack`(bool，默认 `false`)：在稳定提示前缀中包含确定性的项目上下文包(大型美化打印的目录列表)(#4781)。对弱工具调用模型有用；模型可以用一次 `File` 调用重建同样信息。
  - 已移除的 seam 管理器键(`enabled`、`verbatim_window_turns`、`l1_threshold`、`l2_threshold`、`l3_threshold`、`seam_model`)如仍留在旧配置中会被忽略，不再加载。

- `compaction.*`（可选，config.toml）：压缩一旦触发，一次压缩执行的行为。`auto_compact` / `auto_compact_threshold_percent`（settings.toml）仍决定它*何时*触发。这两个键默认都不存在，不存在即表示内置行为，保持不变：
  - `[compaction].summary_instructions`（字符串，默认为空）：常驻的操作者指令，会在**每一次**压缩（手动和自动）时，作为一个清晰分隔的“Additional instructions from the operator”小节附加到摘要器提示中——它是一次性 `/compact <focus>` 的常驻版本，而 `/compact <focus>` 仍会排在这段文字之后。适用于“总是列出确切的文件路径和行号”“总是重述未决的决定及其权衡”“总是先写 TL;DR”这类要求。超过 4 000 个字符会被截断，并给出指名该键的警告；仅含空白视为未设置。摘要器仍在没有系统提示、没有工具的情况下运行——这段后缀是它看到的唯一由操作者撰写的输入。
  - `[compaction].retained_user_message_tokens`（int，默认 `20000`，钳制到 `2000`–`200000`；也接受 `retained_user_message_max_tokens`）：替换后的历史中**原样**保留的近期普通用户消息的 token 预算。调高它会保留更多用户自己较早的消息，而不是只保留有损摘要碰巧捕获的内容；最后一轮的留存契约仍在此基础上适用。`/compact` 回执会指出生效的预算，以及是否应用了操作者指令，这样你能判断该旋钮是否生效。

- `retry.*`(可选):API 请求的重试/退避设置：
  - `[retry].enabled`(bool，默认 `true`)
  - `[retry].max_retries`(int，默认 `3`)
  - `[retry].initial_delay`(float 秒，默认 `1.0`)
  - `[retry].max_delay`(float 秒，默认 `60.0`)
  - `[retry].exponential_base`（float，默认 `2.0`）
  - `[retry].jitter`（bool，默认 `true`）：为每次退避延迟加入随机变化
  - `[retry].jitter_factor`（float，默认 `0.1`，即 ±10%；钳制到 `0.0..=1.0`）
  - `[retry].respect_retry_after`（bool，默认 `true`）：按服务器的 `Retry-After` 响应头等待，而不是使用计算出的退避延迟

  `[retry]` 安排客户端内部的 HTTP 请求重试。每次实际发出的流式请求都有独立的 `stream.open_timeout_secs` 响应头等待时间；提供方退避和遵循的 `Retry-After` 等待发生在请求之间，不占用这个时间，因此可以执行配置的重试次数。默认客户端遇到已分类的传输错误或响应头停滞时，可在该次尝试中改用 HTTP/1.1 一次；固定使用 HTTP/1.1 时跳过协议回退。两种客户端采用相同的 HTTP 状态分类和重试策略。明确的套餐周期用量耗尽会作为配额错误终止。提供方已接受请求之后发生的传输故障仍有歧义，重试可能产生另一笔生成费用。位于客户端之上的流级预算——一个回合对未能打开或中途断掉的流重新发起多少次请求——由下面的 `[stream]` 键控制；旧的 `tui.stream_max_*` 键仍作为回退。

- `[notifications]`：通知的投递、注意力、类别和声音共用同一套策略。`quiet = true`、`method = "off"`、`condition = "never"` 以及被禁用的类别，会同时抑制横幅和 Codewhale 所选的声音。
- `notifications.method`：`auto`（默认）、`osc9`、`kitty`、`ghostty`、`bel`、`off`。
- `notifications.condition`：`unfocused`（默认）、`always`、`never`。缺省时，旧的 `tui.notification_condition` 仍是回退。`always` 还会绕过时长阈值；`unfocused` 要求已离开两秒。
- `notifications.threshold_secs`：非负整数，默认 `30`。
- `notifications.include_summary`：布尔值，默认 `false`。
- `notifications.sound`：可选 `off`、`whale`、`bell`、`beep`、`file`。所选的值控制所有已启用类别的音频。缺省则保留旧的 `completion_sound` 和 `event_sound` 选择；`off` 会同时覆盖这两者。
- `notifications.sound_file`：`sound = "file"` 或旧的 `completion_sound = "file"` 所用的自定义本地 WAV 路径。
- `notifications.subagent_completion`：`always`、`final-only`（默认）、`off`。涵盖所有结束的后台工作：子智能体、后台 shell 和持久任务。`final-only` 会在不再有智能体、workflow 或持久任务仍在运行时，发送一条指名所有已结束事项的通知；仍在运行的 shell（开发服务器、文件监视器）从不会拖住它。`always` 则每一项发送一条。
- `notifications.quiet`：布尔值，默认 `false`。
- `notifications.events`：六个布尔类别，默认全部启用；见下文。
- `notifications.completion_sound`：旧的完成提示，默认 `off`，取值与 `sound` 相同。仅在缺少 `sound` 时使用。
- `notifications.event_sound`：旧的 `enabled`（默认 `false`）、`events`（默认 `["turn-complete", "approval-needed"]`）和 `quiet`（默认 `false`）。`min_interval_ms`（默认 `2000`）在两种模式下都适用于每个类别的音频。

- `tui.alternate_screen`(字符串，可选，默认 `auto`)：交互式会话启动时使用哪个屏幕。`auto` 和 `always` 在 TUI 拥有的备用屏幕上启动；`never` 以内联模式启动——与终端等高、不使用备用屏幕的 ratatui 视口，因此 shell 的回滚缓冲在会话期间保持完好，退出后仍可滚动。`/fullscreen` 与 `/inline` 在进程内切换；终端拒绝的切换会回滚并说明原因。内联模式在其视口内绘制整个转录——会话运行期间不会向宿主回滚缓冲写入任何内容。

- `tui.mouse_capture`（bool，可选，在非 Windows 终端，以及备用屏幕活动时的 Windows Terminal/ConEmu/Cmder 上默认 `true`；在旧 Windows 控制台和 JetBrains JediTerm 内部——PyCharm/IDEA/CLion 等——为 `false`，那里鼠标事件转义会作为乱码文本漏进输入流，见 #878 / #898）：启用内部鼠标滚动、转录选择、右键上下文动作和转录滚动条拖动。TUI 拥有的拖拽选择会复制被选中的单元格，从段落中移除视觉换行列产生的断行，并使选择限于转录窗格；默认复制的内容是 Markdown 源码，见下面的 `tui.selection_copy_markdown`。设为 `false` 或带 `--no-mouse-capture` 运行，即使用终端原始选择；设为 `true` 或带 `--mouse-capture` 运行，可在任何默认关闭的地方选择加入。在原始终端选择下，尤其是旧 Windows 控制台或鼠标捕获被禁用时，选择可能跨越右侧任务面板并包含视觉换行，因为选择由终端而不是 TUI 拥有。
  在 Linux 上，结束一次转录或输入区选择时，会悄悄把文本复制到 PRIMARY，常规剪贴板保持不变。在输入区内中键点击会在指针处粘贴 PRIMARY，但不会提交。这使用原生的 X11 或 Wayland 数据控制；合成器必须支持 PRIMARY 选择。在没有转发图形显示的 SSH 上，请使用终端自己的选择/粘贴手势或 `--no-mouse-capture`。显式的复制仍使用常规剪贴板。

- `tui.selection_copy_markdown`（bool，可选，默认 `true`）：把 TUI 拥有的转录选择（拖拽释放、上下文菜单中的复制，以及对活动选择的 `Cmd+C`/`Ctrl+C`）复制为 Markdown 源码，而不是渲染后的文本。每个被选中的单元格都经过 `Ctrl-Y` 和 `/copy` 所用的同一套规范投影来序列化——用户和助手单元格保留其撰写的 Markdown，其他单元格保留完整的转录形式——部分相交的单元格会补全为整个单元格，各单元格之间以空行连接，并有一条 toast 指出复制了多少个单元格。设为 `false` 则按显示的样子复制渲染后的文本。输入区的选择和 Linux 的 PRIMARY 自动复制不受影响；PRIMARY 始终携带渲染后的文本。

- `tui.stream_chunk_timeout_secs`(int，可选，默认 `900`)：流式模型响应的每 SSE 块空闲超时。慢的本地或兼容服务器可以用 `/config stream_chunk_timeout_secs <seconds>` 提高；`0` 映射到默认，显式值必须 `1..=3600`。省略该键时旧 `DEEPSEEK_STREAM_IDLE_TIMEOUT_SECS` 环境变量仍被遵循。

- `tui.osc8_links`(bool，可选，macOS/Linux 默认开启，Windows 默认关闭)：在转录输出的 URL 周围发出 OSC 8 转义序列，这样支持的终端(iTerm2、Terminal.app 13+、Ghostty、Kitty、WezTerm、Alacritty、较新的 gnome-terminal/konsole)可以用终端的链接手势打开它们——通常是 macOS 上的 Cmd-click,Linux/Windows 上的 Ctrl-click。没有 OSC 8 支持的终端渲染普通标签并忽略转义。转义带外发出(不在缓冲区单元格内)，所以列损坏不是问题；只在终端错误渲染 OSC 8 终止符本身时设 `false`。Windows 旧控制台默认关闭；用 `true` 选择加入。

- `tui.max_model_steps`（int，可选，默认不设上限）：单个普通回合的可选模型步数上限。省略或为 `0` 表示模型步数不设上限；显式的正值会原样保留（`4294967295` 是表示不设上限的兼容值）。无头 `exec` 和 Fleet worker 同样没有隐含的模型步数上限；`exec --max-turns N` 和正数的 worker 预算仍然适用。步数耗尽时，回合以 `Failed` 结束，信息为 `Maximum model steps reached before completion (limit: N)`，必要时会先有一次有界的最终报告响应。累计的墙钟时间和逐流的限制保持独立。处于活动状态的交互式目标回合改用 `goal.max_steps`（同样默认不设上限）；见下文的目标循环一节。

- `tui.turn_wall_clock_secs`（int，可选，默认不设限）：每个回合累计的墙钟预算，单位为秒，跨一个回合的每个模型步骤度量（不是按请求）。等待人工审批所阻塞的时间不计入。省略或为 `0` 表示不设限；正值会原样保留，不再附加运行时上限。耗尽时，回合会在授权下一次计费请求之前停止，并给出一条指明该限制及要调高的键的信息。

- `tui.stream_max_resumes`（int，可选，默认 `3`）：一个回合在其流失败之后，重新发起模型请求的次数——请求从未打开（连接失败或响应头停滞）、流在任何内容之前就断了、主机在流中途休眠，或网络在流中途断开。所有这些路径都消耗同一份预算，而健康的流会重置它。`0` 禁用回合级的重新发起（此时失败的流会使回合失败）；值钳制到 `0..=10`。

- `tui.stream_max_transparent_retries`（int，可选，默认 `2`）：尚无任何内容流出时的流内重新请求。`0` 禁用它们；值钳制到 `0..=10`。

- `tui.stream_max_errors`（int，可选，默认 `5`）：一条流在结束之前所容忍的可恢复错误数。与上面两个重试次数不同，`0` 并不会关闭任何东西：和其他有限的流预算一样，它选择默认值。其他值钳制到 `1..=50`，所以 `1` 会在第一个可恢复错误时结束该流。

- `tui.stream_open_timeout_secs`（int，可选，默认 `45`）：等待流式请求响应头的时间（含连接建立）。HTTP/2 上的响应头停滞会以同样的等待时间在 HTTP/1.1 上重试一次。省略或为 `0` 时回退到 `CODEWHALE_STREAM_OPEN_TIMEOUT_SECS`，再回退到默认值；值钳制到 `5..=300`。

- `tui.connect_timeout_secs`（int，可选，默认 `30`）：模型 HTTP 客户端的 TCP/TLS 连接超时。省略或为 `0` 使用默认值；值钳制到 `1..=300`。

- `tui.force_http1`（bool，可选，默认 `false`）：将模型 HTTP 客户端固定为 HTTP/1.1，适用于无法正确处理长期 HTTP/2 流的提供商边缘服务或代理。`CODEWHALE_FORCE_HTTP1=1` 的作用相同；任一项启用即可固定协议。

- `tui.stream_max_content_mb`（int，可选，默认 `10`）和 `tui.stream_max_duration_secs`（int，可选，默认 `1800`）：单条流累计内容与墙钟时长的逐步上限。`0` 选择默认值；值分别钳制到 `1..=512` MB 和 `10..=86400` 秒。

- `transcript.prose_measure`(正整数，可选，默认缺省 = 全宽)：实时转录中散文单元格——用户消息、助手回答和推理/思考块——的换行上限，以列为单位(#5436)。缺省(或 `0`)使用全部内容宽度，与工具/状态单元格和 #5322 宽帧决策一致；前 105 列散文栏已移除。在超宽终端上设置正整数(例如 `[transcript]` 下的 `prose_measure = 120`)恢复有界的阅读度量。窄终端总是保持内容宽度——上限只从上方钳制。工具、diff 和状态单元格从不继承这个上限。无效值(负或非整数)在启动时以 `transcript.prose_measure` 配置错误被拒绝。每次渲染遍解析一次，所以主转录缓存和全屏覆盖层总是就有效宽度达成一致。

- `hooks`(可选)：生命周期 hooks 配置(见 `config.example.toml`)。

- `features.*`(可选)：功能标志覆盖(见下文)。

### 流与传输设置

`[stream]` 是模型流策略及其 HTTP 客户端的规范配置表。
`codewhale config dump` 和 `codewhale config get stream` 显示运行时解析后的有效值，
包括环境变量回退及范围限制；查看不会把默认值写入文件。例如：
`codewhale config set stream.open_timeout_secs 120`、
`codewhale config unset stream.open_timeout_secs`。单次运行的
`--set stream.open_timeout_secs=120` 使用同一验证器。

| `[stream]` 键 | 默认值 | 有效行为 | 旧 `[tui]` 回退键 |
| --- | --- | --- | --- |
| `open_timeout_secs` | 45 | 正数限制到 5–300；0 或省略时回退到环境变量/默认值 | `stream_open_timeout_secs` |
| `chunk_timeout_secs` | 900 | 0 使用默认值；正数限制到 1–3600 | `stream_chunk_timeout_secs` |
| `max_resumes` | 3 | 0 禁止重新发起整个请求；最大 10 | `stream_max_resumes` |
| `max_transparent_retries` | 2 | 0 禁止输出任何内容前的重试；最大 10 | `stream_max_transparent_retries` |
| `max_stream_errors` | 5 | 0 使用默认值；正数限制到 1–50 | `stream_max_errors` |
| `max_duration_secs` | 1800 | 每条流的总时长；0 使用默认值，正数限制到 10–86400 秒 | `stream_max_duration_secs` |
| `max_content_mb` | 10 | 每条流的内容上限；0 使用默认值，正数限制到 1–512 MiB | `stream_max_content_mb` |
| `connect_timeout_secs` | 30 | TCP/TLS 建连；0 使用默认值，正数限制到 1–300 秒 | `connect_timeout_secs` |
| `force_http1` | false | 布尔值；环境变量固定为真时始终启用 HTTP/1.1 | `force_http1` |
| `tcp_keepalive_secs` | 30 | TCP 保活探测前的空闲时间；0 禁用，正数限制到 1–3600 秒 | 无 |
| `http2_keep_alive_interval_secs` | 15 | 活跃 HTTP/2 连接的 PING 间隔；0 禁用，正数限制到 1–3600 秒 | 无 |
| `http2_keep_alive_timeout_secs` | 20 | PING 确认期限；0 使用默认值，正数限制到 1–3600 秒 | 无 |

每个显式规范字段都优先于旧 `[tui]` 字段，包括 `0` 与 `false`；省略规范字段则保留旧值。
正数建连响应头等待时间优先于 `CODEWHALE_STREAM_OPEN_TIMEOUT_SECS`（再回退到
`DEEPSEEK_STREAM_OPEN_TIMEOUT_SECS`）；0 回退到这些变量。省略分块超时会使用
`CODEWHALE_STREAM_IDLE_TIMEOUT_SECS`（再回退到 `DEEPSEEK_STREAM_IDLE_TIMEOUT_SECS`），
显式 0 则使用 900。`CODEWHALE_FORCE_HTTP1`（旧名 `DEEPSEEK_FORCE_HTTP1`）
与配置做逻辑或，所以配置 `false` 不能取消环境变量固定的 `true`。Profile 逐字段合并此表。

传输设置只影响新建的模型、目录和 HTTP/1 回退客户端，不会重建活跃客户端，
也不控制 MCP 或其他网络服务。HTTP/2 PING 不作用于空闲连接池；固定 HTTP/1.1
时 HTTP/2 设置无效。操作系统控制 TCP 探测细节；这些键不会关闭证书校验。
`[retry]` 仍独立拥有 HTTP 请求退避计划。

### 工作区笔记

`/note` 在当前工作区的 `.codewhale/notes.md` 管理一个简单的笔记文件(旧 `.deepseek/notes.md` 是尚不存在 `.codewhale/notes.md` 时的回退路径)。现有的 `/note <text>` 用法仍追加笔记。管理形式：

| 命令 | 动作 |
|---|---|
| `/note <text>` | 追加笔记(旧简写) |
| `/note add <text>` | 显式追加笔记 |
| `/note list` | 列出笔记，带临时的从 1 开始编号 |
| `/note show <n>` | 显示编号 `n` 的完整笔记 |
| `/note edit <n> <text>` | 用新文本替换笔记 `n` |
| `/note remove <n>` | 删除笔记 `n`；`rm` 和 `delete` 是别名 |
| `/note clear` | 清空工作区笔记文件 |
| `/note path` | 显示解析出的工作区笔记路径 |

`/note list` 显示的编号不存储在文件中；它们每次读取笔记时从当前顺序派生。这让文件格式与现有的 `---` 分隔笔记兼容。

### 用户记忆

用户记忆拆分为一个顶层路径设置和一个选择加入开关表：

```toml
# 只锚定存储——实际写入发生在
# ~/.codewhale/memory/global/MEMORY.md(见 MEMORY.md)。
memory_path = "~/.codewhale/memory.md"

[memory]
enabled = true
```

注意：

- `memory_path` 保持在顶层，紧挨 `notes_path` 和 `skills_dir`；它不嵌套在 `[memory]` 下。
- 配置的路径是**锚点**：它的父目录获得 `memory/global/MEMORY.md`、工作区作用域文件和 `index.db`。把 `memory_path` 指向原生布局路径本身会双重嵌套(`…/memory/global/memory/global/MEMORY.md`)；保持旧式锚文件名。
- `DEEPSEEK_MEMORY_PATH` 从环境覆盖锚路径。
- `DEEPSEEK_MEMORY=on`(也 `1`、`true`、`yes`、`y` 或 `enabled`)不编辑 `config.toml` 就翻转功能开启。
- 禁用时功能惰性：不注入文件，`# foo` 落到普通消息提交，模型看不到 `remember` 工具。
- 示例和完整 `/memory` 命令面见 [`MEMORY.md`](./MEMORY.md)。

### 目标循环（`[goal]`）

Operate 模式的目标会一直运行到完成门，默认没有 token、时间或继续次数的上限（#5052）。显式提供的 token/时间预算只是遥测，不会停止目标。想要断路器的用户可以选择加入一个：

```toml
[goal]
# Optional safety backstop on automatic goal continuation passes.
# Default: 0 (unlimited). Set a positive value to opt into a ceiling.
max_continuations = 100

# Optional cancellable quiet period between successful turns. This is useful
# for coordinator goals that should poll on a cadence instead of keeping one
# provider turn open. Default: 0 (continue immediately).
continuation_delay_seconds = 300

# Optional per-turn model-step ceiling while a goal is active (#6512).
# Default: uncapped (0 or absent). Positive values are preserved.
# This bounds each provider turn, never the number of continuation passes.
max_steps = 0
```

有效延迟上限为 86,400 秒（24 小时）；比每天一次更不频繁的调度，请使用自动化。

显式的兜底触发时，目标会以一条点名 `[goal] max_continuations` 的状态消息暂停，并记录一条警告；检查进度后再恢复目标，或者调高/禁用该兜底。

`[goal] max_steps` 一次只约束一个引擎回合。与普通的交互式回合一样，省略或设为 `0` 表示模型步数不设上限。显式的逐次调用上限——`exec --max-turns N`、子 worker 的上限——始终优先于它。显式预算耗尽时，它会得到一次有界的最终报告，并且该回合被归类为预算耗尽。未完成的目标随后会以 BudgetLimit 原因暂停，而不是重新启动下一个目标回合——请在审阅报告之后显式恢复它。墙钟时间和流保护是独立的，仍然适用。

延迟只在显式创建的目标仍然活动、且在一个成功回合之后才开始。`/goal pause`、`/goal done`、`/goal blocked`、`/goal clear`、Esc 或 Ctrl+C 会在下一次 provider 请求开始之前取消待定的继续。失败的回合和策略/路由失败绝不会安排下一个回合。配置中只存储数字形式的节奏；该循环不会持久化任何提示、凭据或机密。

### 仅推理恢复（`[reasoning_only]`）

当推理模型（思考模式）以只有隐藏推理、没有答案文本或工具调用的方式结束一次响应时，引擎可以自动重新请求答案。用 `[reasoning_only]` 表配置这一行为：

```toml
[reasoning_only]
# Maximum number of automatic re-requests. Default: 2.
# Set to 0 to disable automatic recovery (fail immediately).
max_reprompts = 10

# Optional custom message sent to the model on each re-request.
# When set, overrides the built-in default message.
# When unset (or commented out), the engine uses:
#   "So, what's up ? Keep running !"
reprompt_message = "Allez, répond quelque chose !"
```

这只适用于模型返回干净的 `stop` 结束原因、且只有思考内容的情况。因输出长度而停止（`length`/`max_tokens`）绝不会重试，而一个始终没有答案的模型，在达到配置的上限后仍会如实失败。

要完全禁用重新提示消息（静默重试），把它设为空字符串：

```toml
[reasoning_only]
reprompt_message = ""
```

### 通知

通知控制项可以在现有的 `/config` 设置视图、终端命令以及 CLI 中使用。它们写入的是同样的 `config.toml` 键。终端中的更改立即生效；加上 `--save` 才会保留。CLI 写入在下一个进程加载配置时生效。

```sh
codewhale config set notifications.sound whale
codewhale config set notifications.events.approval-needed false
codewhale config set notifications.quiet true
codewhale config get notifications
codewhale config unset notifications.quiet
```

```text
/config notifications sound whale --save
/config notifications condition unfocused --save
/config notifications quiet true
/config notifications status
```

嵌套的 CLI 编辑会保留 TOML 类型、无关的键和注释。unset 只删除所选的叶子键。TUI 中的 `notifications.sound legacy`，或对 `notifications.sound` 执行 CLI unset，会恢复此前的声音选择。无效的值会在改动文件或会话之前被拒绝。在活动的 TUI profile 中，已保存的编辑会更新该 profile 自己拥有的通知表；如果没有，则更新继承来的根表。设置详情会把已保存值和当前值区分开。

```toml
[notifications]
method = "auto"        # auto | osc9 | kitty | ghostty | bel | off
condition = "unfocused" # unfocused | always | never
threshold_secs = 30
include_summary = false
sound = "whale"        # optional; sound is opt-in, not enabled by default
quiet = false

[notifications.events]
turn-complete = true
subagent-terminal = true
approval-needed = true
input-needed = true
elevation-needed = true
model-notify = true
```

`quiet = true` 会让所有类别静音，而不改变已保存的选择。`method = "off"` 同样会同时停止横幅和所选的音频。禁用某个类别会停止它的声音。注意力和时长闸门在任何输出通道运行之前就已生效。标题动画的完成是静默的；只有经过授权的通知事件才能请求音频。回合成功完成是一个通知类别；失败/取消的回合不会产生成功通知。

`auto` 会选择一个可识别的终端协议，或现有的 macOS 原生回退；未知终端仍然不受支持，也绝不会凭空发出铃声。`bel` 是纯音频传输：只派发一次所选的提示音，不会再有第二声传输铃。`sound = "off"` 时，该传输是静默的。`osc9`、`kitty` 和 `ghostty` 使用它们各自的终端通知协议；tmux 透传得到保留。终端/OS 的通知偏好仍然决定显示、归属，以及宿主自己添加的任何声音。

默认情况下，终端必须保持失去焦点两秒。`condition = "always"` 允许前台通知，并绕过时长阈值；`"never"` 抑制所有投递。规范的 condition 优先于旧的 `[tui].notification_condition`。

随附的 `whale` 是一段 1.55 秒的、受鲸鱼启发的原创提示音，不含任何第三方录音。它仍是等待试听批准的选择加入候选项。WAV 播放使用后台工作线程：macOS 的 `/usr/bin/afplay`、Linux 上来自 [ALSA utilities](https://github.com/alsa-project/alsa-utils) 的 `aplay`，或 Windows 的 `PlaySoundW`。缺少播放器/文件或平台不受支持时，没有回退铃声。同一时间只播放一个 WAV。工作线程启动的回执只是一次派发尝试，并不证明声音确实播放或被 OS 接受。现有的 macOS `osascript` 横幅仍保留 Script Editor 归属；这次 Core 更改并不提供带品牌的原生 Apps 横幅。

#### 此前的声音设置

当 `notifications.sound` 缺失时，完成事件使用非 off 的 `completion_sound` 选择，其他事件使用现有的事件允许列表。它们是同一个音频决策的兼容输入，而不是各自独立的播放路径。选择了全局声音时，它优先于此前的完成/事件选择。默认仍然是静音，除非此前已经选择了旧版声音或显式的 `bel` 传输。

```toml
[notifications.event_sound]
enabled = false
events = ["turn-complete", "approval-needed"]
min_interval_ms = 2000
quiet = false
```

旧版事件提示对完成、子智能体完成、输入和模型通知使用一声铃；审批/提权提示使用两声。每个类别的重复间隔在设置刷新后仍然保留。旧的未知事件名在加载时被忽略；新的 CLI/TUI 编辑要求使用上面六个类别中的名称。

当外部通知被静音时，本地的审批/输入/提权提示和错误回执仍然可用。操作提示使用所选的界面语言，并在该请求结案时撤下。反复出现的实时通知保留其最初的过期时间；之后的例行更新不会在完成时隐藏一个尚未解决的警告。可选的插件建议 toast 和上下文提示共享同一个会话引导预算。`/config contextual_tips off` 会隐藏这些 toast，同时保留必要的通知和显式的插件审查请求。

#### 通知可以包含什么

桌面通知是扫一眼的界面：在 macOS 上它可以出现在锁屏上，在每个平台上它对机器附近的任何人都是可见的。因此 Codewhale 是从带有固定的逐事件披露策略的类型化负载来构建通知的，而不是取手头碰巧有的文本：

| 事件 | 显示 | 绝不显示 |
|---|---|---|
| 回合完成 | 状态行（`include_summary` 时加上耗时/成本）、助手回复的预览 | — |
| 子智能体完成 | 状态行、智能体 id、子智能体摘要行的预览 | — |
| 需要审批 | 工具名 | 工具描述、命令、参数 |
| 需要输入 | “在终端回答该问题以继续” | 问题本身 |
| 需要沙箱提权 | 工具名和拒绝原因 | 命令 |
| `notify` 工具 | 模型提供的标题和正文 | — |

每个字段都有上限（状态行 80 个字符，标识符 120，预览 200），会剥离控制字节和转义序列，并经过一个脱敏器：它把形似凭据的字符串替换为 `[redacted]`，把本地绝对路径缩减为 `…/basename`，并把原始工具 JSON 替换为 `[details hidden]`。这个脱敏器刻意过于积极：一段 40 个字符的连续字符串没有词的结构，所以即使它不是机密也会被脱敏。

#### macOS：为什么横幅显示 “Script Editor”

在自身不提供通知转义的 macOS 终端上——Apple Terminal、VS Code 和 JetBrains 内嵌终端、没有 `LC_TERMINAL` 的普通 tmux——`method = "auto"` 会回退到 `osascript` 的 `display notification`。该命令是代表*已打包的*宿主进程发出通知，而 `/usr/bin/osascript` 没有打包，所以 macOS 把横幅归因于 `com.apple.ScriptEditor2`。这种归属提供了 Script Editor 的图标，并拥有“系统设置 → 通知”里的条目（提醒样式、预览、勿扰）。`display notification` 没有图标参数，所以无法从通知代码里修复；这需要 Codewhale 发布真正的 `.app` 包。已在 [#4834](https://github.com/codewhale-hq/CodeWhale/issues/4834) 中跟踪。在此期间，iTerm2、WezTerm、Ghostty 和 kitty 会被优先匹配，使用它们自己的通知协议，而 `method = "osc9"` / `"bel"` / `"off"` 可以显式退出 `osascript` 路径。

## 终端中的自动化

打开 `/automation`，然后选择 **New automation**（`n`）或 **Edit automation**（`e`）。该表单编辑名称、多行提示、调度、模型、工作区和启用状态。`Tab` 在字段之间移动；`Enter` 在提示中插入换行。调度预设包括每天、每周、每小时、一次性，以及自定义的 RRULE。时间字段接受 `HH:MM`；它们的方向控件以 15 分钟为单位调整时间。每周的星期按钮和模型搜索同时支持鼠标和键盘。

下一次运行的预览使用调度器的本地时区，并显示其 UTC 偏移。已启用的自动化在 **Save**（`Ctrl+S`）之后即可运行；暂停的自动化没有计划中的下一次运行。**Cancel**（`Esc`）会丢弃草稿。除非明确更改了对应的受支持字段，编辑会保留现有的权限设置、自定义调度和额外的工作区条目。

调度按机器的本地时区，对照挂钟时间来计算。在春季拨快时钟的那一天里不存在的时刻会被跳过，而秋季拨回时出现两次的时刻只触发一次。Codewhale 关闭、休眠或仍在运行上一次触发期间错过的触发会被合并：下次启动时运行一次补跑，然后从下一个未来的时间点继续，绝不会重放每一个错过的时间点。当同一个自动化较早的一次运行仍在排队或运行时，新的触发绝不会开始。每次运行都会持久地记录其状态、时间和错误；需要工具审批的运行没有操作者可问，审批等待过期后就会失败。

选择一个具体模型会同时固定该模型及其确切配置的 provider，包括具名的自定义路由。之后对活动 provider 的更改不会移动该自动化的固定项。选择默认模型，以及没有 provider 固定项的旧定义，则保持运行时现有的默认行为。新的自动化记录使用 schema v2，任务使用 v3，这样较旧的运行时会拒绝那些它们无法保留其 provider 固定项的记录。

## 生命周期发件箱（`[lifecycle_outbox]`）

生命周期发件箱（outbox）是一个选择加入的、机器可读的会话、回合和子智能体生命周期事件流。配置了路径后，Codewhale 会为每个事件向该文件追加一行 JSON——对交互式 TUI 会话*以及*无头的 `codewhale exec` 运行都是如此——这样监督者（终端复用器包装器、自动化 harness、告警设置）无需抓取屏幕，也无需安装逐个 hook 的 shell 命令，就能对发生的事情作出反应。未设置或 `path` 为空 = 该功能**关闭**，行为不变。

```toml
[lifecycle_outbox]
path = "~/.codewhale/notifications/outbox.jsonl" # unset/empty = OFF
webhook_url = ""     # optional; POSTs events as JSON when set
webhook_token = ""   # optional bearer token for webhook_url
```

### 发出的事件

| 事件 | 种类 | 触发时机 |
|---|---|---|
| `turn_start` | `turn.started` | 新回合开始（TUI 的 TurnStarted；`exec` 在消息派发时） |
| `turn_end` | `turn.completed` / `turn.failed` / `turn.interrupted` | 回合完成，种类由回合状态投影而来 |
| `turn_stalled` | `turn.stalled` | 停滞看门狗恢复了一个卡死的回合 |
| `subagent_spawn` | `subagent.spawned` | 派生了一个子智能体 |
| `subagent_complete` | `subagent.completed` | 子智能体到达终态 |
| `session_start` | `session.started` | 交互式会话开始 |
| `session_end` | `session.ended` | 交互式会话结束 |

### 文件契约

每一行是一个 `RuntimeEventEnvelope`：

```json
{"schema_version": 1, "seq": 3, "event": "turn_start", "kind": "turn.started",
 "thread_id": "…", "turn_id": "…", "item_id": null, "timestamp": "…",
 "created_at": "…", "payload": {…}}
```

- `seq` 对每个发件箱文件单调递增，并在新进程打开该文件时，从最后写入的一行恢复。
- 每次追加写入完整的一行 JSON，由内部写入任务串行化，并在下一个事件之前刷新；写同一路径的并发会话不会在行中间交错字节，但各个独立进程会各自从它们自己恢复的 `seq` 继续，所以共享同一个文件的多个进程之间 seq 可能重复——要严格唯一，请每个进程使用一个文件。
- 父目录会在第一个事件时惰性创建。
- 负载只由有界的、已预先脱敏的字段构造——绝不含原始工具参数、环境或完整的转录文本。自由格式字段（错误消息、预览）按通知的上限截断（标题 80 / 详情 120 / 预览 200 个字符），并剥离控制字节。

### Webhook 投递

设置了 `webhook_url` 时，每个事件还会额外以 `{"at": "<ISO 8601 timestamp>", "event": {…}}` 的形式 POST 出去，配置了 token 时带上 `Authorization: Bearer <webhook_token>`。投递是尽力而为：失败会被记录并丢弃，绝不会重试到智能体循环里，而且失败的 webhook 绝不会阻塞本地文件的追加。

## 控制套接字（`[control_socket]`）

面向受监督运行的每会话控制界面：启用该功能后，交互式 TUI 会为每个*正在运行的*会话，在 `<sessions-dir>/<session-id>/control.sock` 绑定一个 unix 域套接字（权限 `0600`；`<sessions-dir>` 与会话存储所用的目录相同，通常是 `~/.codewhale/sessions`）。该套接字会随会话的产物目录一同删除，而崩溃进程遗留的陈旧套接字会被下一次启动接管。未设置或 `enabled = false` = 该功能**关闭**（默认），行为不变。仅限 Unix；在其他平台上该键可以解析，但不会绑定任何套接字。

```toml
[control_socket]
enabled = false # default: OFF
```

该套接字使用以换行分帧的 JSON-RPC，每个连接一个请求：写入一行请求，读取一行响应，然后关闭。

```json
{"id":"1","method":"message","params":{"text":"hello"}}
{"id":"2","method":"interrupt","params":{}}
{"id":"3","method":"status","params":{}}
```

- `message`——把 `text` 作为结构化的用户消息，经由普通的输入区派发路径投递；空闲时立即派发，回合进行中则排队（响应中的 `delivery` 字段会说明是哪一种）。
- `interrupt`——对活动回合的 Esc 式取消；`cancelled` 报告是否有活动工作正在进行。
- `status`——回答 `turn_state`（`idle` / `in_progress` / `waiting`）和 `goal`（`objective`、`status`、`paused`）。

成功响应会回显请求 id，并带一个以 `type` 标记的结果；失败则携带 `error.code`（`invalid_request`、`command_error`、`timeout`、`server_unavailable`）。请求每行上限 1 MiB，处理程序在 5 秒内没有应答会被报告为 `timeout`。

## 工具目录(Tool Catalog)

Codewhale 默认加载一个小型核心原生工具目录，把不太常见的原生工具留给 ToolSearch 发现。要让特定原生工具在每个请求都加载，把它们加到 `[tools].always_load`：

```toml
[tools]
always_load = ["Git", "notify"]
```

### 脚本工具与覆盖

`~/.codewhale/tools/`（或 `[tools].plugin_dir`）内以 `# name:` 头声明的脚本成为
模型可见工具，`/plugin tools` 列出它们。脚本从 stdin 读取工具 JSON 输入，
向 stdout 写 JSON `ToolResult`，例如 `{"content":"...","success":true}`。

```sh
#!/usr/bin/env sh
# name: word_count
# description: Count words in the given text
# schema: {"type":"object","properties":{"text":{"type":"string"}}}
# approval: required
```

`# approval:` 支持 `suggest`（默认）或 `required`；两者都遵守会话审批设置。
脚本不能自行批准：`approval: auto` 不再受支持，会采用默认值，并在运行时日志
（`~/.codewhale/logs/`）及 `/plugin tools` 中说明。

与已注册工具重名的脚本不会加载。`[tools.overrides]` 可以禁用内置工具，或以
新名称添加 `script` / `command` 工具；不能用它们替换内置名称：

```toml
[tools.overrides]
"Web" = { type = "disabled" }
"audited_shell" = { type = "script", path = "audit-shell.sh" }
"Bash" = { type = "script", path = "audit-shell.sh" } # 拒绝；Bash 仍生效
```

拒绝的键每个会话显示一次状态行并写入日志。要包装内置工具，请禁用它并用新名称
注册包装器。覆盖仍可替换同名的普通脚本；相对 `path` 按插件目录解析。

### `request_user_input` 限制

`request_user_input` 会向用户提出一小批选择题。两个上限都是可配置的（#5949）：当研究或规划工作流确实需要更多澄清时，调高 `user_input_max_questions`；当交互式分诊应保持简洁时，调低它。

```toml
[tools]
user_input_max_questions = 6   # default 6, clamped to 1..=10
user_input_max_options = 4     # default 4, clamped to 2..=10
```

有效值会同时应用在三个地方：工具的 JSON schema（`minItems` / `maxItems`）、它对模型可见的描述，以及负载校验器。被拒绝的负载会指出要调高的键，这样模型既可以调整这一批的大小，也可以告诉用户该更改哪个设置。

### 用户输入等待超时

`request_user_input` 的提问默认一直等待用户回答或取消（#6003）。省略 `user_input_timeout_seconds` 或设为 `0` 都不设置超时；正数则限定本次等待的秒数，超时后取消。大于 86,400 秒（24 小时）的值会被限制为 86,400 秒。无头的 `exec` 运行没有应答者，因此默认不提供 `request_user_input`：模型会报告该工具不存在并直接结束，而不是卡住。

0.10.1 中，TUI 的挂起工具计时器识别真实的用户输入与审批等待（#6872）。等待时
暂停该回合的看门狗；答案回到原调用，无需另输 `continue`。回答后重新计算活动等待
基线。用户取消及正数审批/输入期限仍生效；它不会给正在执行的工具无限时间。

```toml
[tools]
user_input_timeout_seconds = 300   # optional 5-minute limit; omitted or 0 waits indefinitely; max 86400 (24h)
```

这个键只管辖提问的等待。审批有它自己的时钟 `[approval] timeout_seconds`，除非你设置它，否则会无限期等待。其他地方的墙钟时间和流保护不受影响。

## 功能标志(Feature Flags)

功能标志位于 `[features]` 表下，跨 profiles 合并。内置工具默认启用，所以你只需要设置想强制开或关的条目。

```toml
[features]
shell_tool = true
subagents = true
web_search = true # 启用延迟 Web;标志名保留用于配置兼容
apply_patch = true
mcp = true
exec_policy = true
code_mode = true # execute_tools composes MCP/plugin/native calls; false defers it behind tool_search
```

`code_mode` 默认开启：`execute_tools` 从第一个请求起就会公布，嵌套调用与直接调用一样经过同一个权限门（见[工具表面](TOOL_SURFACE.md#代码模式execute_tools)）。设置 `code_mode = false` 可再次把它延后到 `tool_search` 之后。

每个功能标志在功能注册表 [`docs/features.toml`](../features.toml) 中都有对应条目；标志与条目不一致时，测试会失败。

你也可以为单次运行覆盖功能：

- `codewhale --enable web_search`
- `codewhale --disable subagents`

用 `codewhale features list` 检查已知标志和它们的有效状态。原生 `/config` 视图还包含一个只读的**实验性(Experimental)**区，用于实验性功能标志。它显示每个标志的有效启用/禁用状态，以及该状态来自默认还是配置覆盖。在 `[features]` 或 `--enable` / `--disable` 中更改功能标志；`/config` 区是审计面，不是稳定性承诺。目标和工作流预览行可能作为保留条目出现在那里，直到那些工作流在真实门控标志后面毕业。

## 网页搜索 Provider

`web_search` 默认使用无 key 的 Firecrawl。运行时失败或耗尽无 key 配额会通过 DuckDuckGo 和 Bing 可见地降级。中国部署可以显式选择百度、秘塔、火山引擎或受信任的 SearXNG 端点；Codewhale 不从区域设置或模型 provider 猜测地理位置。

配置的 API provider 先被尝试。运行时失败或空结果通过 DuckDuckGo 然后 Bing 可见地降级；结构化搜索回执记录每一步。缺失配置和网络策略拒绝失败关闭，不把查询发送到另一个 provider。

**Provider 原生搜索。** 当路由的 provider 提供自带的网页搜索工具（OpenAI、xAI、Anthropic、DeepSeek、Kimi 等）时，该搜索可以排在已配置 provider 之前。它是在当前路由上的一次独立模型调用。`[search] native` 决定顺序：

- 不设置（默认）：只有在没有配置搜索 provider 时原生搜索才优先；通过 `[search] provider`、`CODEWHALE_SEARCH_PROVIDER` 或 Tavily key 选择的 provider 优先；
- `native = true`：即使固定了 provider，原生搜索也优先；
- `native = false`：从不使用原生搜索。

原生搜索的引用在返回前会做链接检查。每个被引用的 URL 都经过与 `Web` fetch 相同的受保护抓取（SSRF、DNS 与重定向检查，以及 `[network]` 策略）：先发 `HEAD`，`HEAD` 被拒绝时再发一个只取一个字节的范围 `GET`，同时最多四个，每个五秒。请求使用该地址在 `results` 中呈现的形式，不含 userinfo、片段和类似凭据的查询参数；查询字符串里重复了搜索词的地址不会被请求，其引用会被移除。只有返回 2xx 状态的来源才会留在 `results` 中；回执把每个被移除的引用记为 `citation_unverified` 并写明原因。如果没有任何引用通过检查，或者查询要求 `recency`（没有原生搜索工具能强制时间窗口），搜索会回落到已配置的 provider。`[network]` 策略拒绝的主机，其引用会被移除；策略需要你确认的主机不会被请求，其引用保留在 `results` 中，回执记为 `citation_unchecked`，直到你允许该主机。

provider 的文字回答完整放在单独的 `unverified_synthesis` 字段中：它是 provider 模型自己的总结，其中的论断和链接都未经检查。过大的工具输出会溢出到会话 artifact，模型可以分页取回。

**时效与区域。** `recency` 和 `locale` 会在后端 API 支持时转发，搜索回执会报告每一项是否生效：

| 后端 | 时效 | 区域 |
| --- | --- | --- |
| Firecrawl | `tbs=qdr:d/w/m/y` | 从地区部分取 `country`（`de-DE` → `DE`）；只有语言时忽略 |
| Tavily | `time_range` | 不发送（Tavily 使用国家全名） |
| SearXNG | `time_range` | `language`，原样发送 |
| Serply | 不发送（文档未说明） | `hl` 语言，`gl` 国家 |
| DeepSeek | 不发送 | 不发送（`localization` 需要 IP 地址） |
| Provider 原生 | 拒绝；搜索回落到已配置的 provider | 不发送 |

时效会向上取整到后端最接近的窗口（天、周、月、年），所以 `recency = 10` 会搜索最近一个月。其他后端忽略这两项，并在回执中说明。

对服务 DuckDuckGo 兼容 HTML 的私有/内部搜索服务，保持 `provider = "duckduckgo"` 并设置 `base_url`；Codewhale 把 `q` 查询参数追加到该端点，并把网络策略应用到它的主机。自定义端点不回退到公共 Bing。`CODEWHALE_SEARCH_BASE_URL` 可按进程覆盖；`DEEPSEEK_SEARCH_BASE_URL` 仍作为旧别名接受。

**SearXNG**([docs](https://docs.searxng.org/dev/search_api.html))使用配置实例的 JSON API。设置 `provider = "searxng"` 和 `base_url = "https://your-searxng.example"`；Codewhale 调用 `/search?q=...&format=json`。Codewhale 默认不使用公共 SearXNG 实例，因为公共实例常禁用 JSON 输出或对 API 流量限速。

把它作为独立进程自托管（Docker 即可）；Codewhale 不内嵌也不管理搜索引擎本身：

- 在实例上启用 JSON（`settings.yml` 的 `search.formats` 必须包含 `json`）并重启。仅 HTML 的实例会对 API 返回 HTTP 403；Codewhale 会把它作为 SearXNG 这一跳的 JSON/API 访问问题报告出来，而不是静默返回空结果。
- 绑定到 loopback，或绑定到你的网络策略允许的主机和端口。实例自带引擎列表、限流器和限额。
- 用 `[search] provider = "searxng"` 和 `base_url` 指向它（必填；根 URL 或 `/search` 端点均可）。不随任何默认实例发布，也不会自动发现实例。
- `codewhale doctor --probe-search` 只对该 origin 发送一个传输层 `HEAD`——不带 `q=`、不带凭据、不跟随重定向、不写审计回执——所以探测通过只证明可达性和网络策略放行，不证明 JSON 已开启。

在假定是 Codewhale 的 bug 之前，先用直接请求确认 JSON API 本身：

```sh
curl -sS "$BASE/search?q=codewhale&format=json" | jq '.results[0] | {title,url,score}'
```

Codewhale 按 `score` 从高到低排序返回行，再对排序结果应用 `max_results`；实例未给出可用分数的行保持原始相对顺序。

**秘塔(Metaso)**([metaso.cn](https://metaso.cn))需要用户提供的 key。设置 `METASO_API_KEY` 或 `[search] api_key`；Codewhale 不提供共享 key。

**Firecrawl**([docs](https://docs.firecrawl.dev/sdks/cli))用其有界的按 IP 每日配额无 key 搜索 Firecrawl Cloud。设置 `FIRECRAWL_API_KEY` 或 `[search] api_key` 用于认证限额。Codewhale 在无 key 模式不发送 `Authorization` 头。

**百度(Baidu)**使用百度 AI 搜索 `https://qianfan.baidubce.com/v2/ai_search/web_search`。设置 `BAIDU_SEARCH_API_KEY` 或 `[search] api_key`。这只是搜索工具后端；它不添加百度模型 provider。

**Sofya**([sofya.co](https://sofya.co))返回完整提取页面内容而不是片段。把 `[search] api_key` 设为你的 `ay_live_...` key，或用 `SOFYA_API_KEY` 环境变量。这只是搜索工具后端；它不添加 Sofya 模型 provider。

**Serply**([serply.io](https://serply.io))返回 Google 自然搜索结果，包含标题、URL 和摘要片段。把 `[search] api_key` 设为你的 Serply key，或用 `SERPLY_API_KEY` 环境变量。这只是搜索工具后端；它不添加 Serply 模型 provider。

**DeepSeek**([docs](https://api-docs.deepseek.com/guides/search))使用 `POST https://api.deepseek.com/v1/tools/search`。设置 `provider = "deepseek"`（也接受 `deep-seek` 和 `deepseek-search`），并依次从 `[search] api_key` 或 `DEEPSEEK_API_KEY` 环境变量读取 key。此搜索后端不复用已保存的 DeepSeek 模型 provider key。查询截断为 128 个 Unicode 字符。后端把数量限制在 1–50；共享搜索工具目前每次最多请求 10 条结果。`site:`、`lang:`、`after:`、`before:` 等查询运算符原样传递；`recency` 和 `locale` 两项被忽略。结果包含标题、URL 和摘要片段；摘要片段为空时回退到 summary。

**Tavily**([tavily.com](https://tavily.com))在存在 Tavily key 且没有固定 provider 时自动选中：设置了 `TAVILY_API_KEY`，或 `[search] api_key` / `CODEWHALE_SEARCH_API_KEY` 属于 `tvly-` 家族。doctor 会把这种情况报告为 `source: tavily key`。自动检测只在运行时生效——Codewhale 不会为它写入 `[search] provider`，也不会把 `TAVILY_API_KEY` 合并进 `[search] api_key`。显式的 `[search] provider` 或 `CODEWHALE_SEARCH_PROVIDER` 始终优先，所以即使环境里有 Tavily key，`provider = "firecrawl"` 仍然是 Firecrawl。固定为 `tavily` 时接受任何非空的 `[search] api_key`，由该 key 或 `TAVILY_API_KEY` 提供配置；两者都为空时直接失败关闭。

```toml
[search]
provider = "firecrawl" # 也 duckduckgo | bing | tavily | bocha | metaso | searxng | baidu | volcengine | sofya | serply | deepseek
# base_url = "https://search.example/" # provider = "duckduckgo" 时可选;"searxng" 时必填
# api_key = "YOUR_KEY" # firecrawl 可选;其他 API 提供商必填
# native = false # provider 原生搜索:不设置 = 仅在未配置 provider 时使用
```

## 本地媒体附件

在输入区用 `@path/to/file` 给下一条消息添加本地文本文件或目录上下文。本地图像/视频媒体路径用 `/attach <path>`，或 `Ctrl+V` 从本地剪贴板或显式转发的 X11/Wayland 剪贴板附加图像。没有转发图形显示的 SSH 终端粘贴仅文本；使用本地终端的粘贴命令(macOS 的 `Cmd+V`，Linux/Windows 的 `Ctrl+Shift+V`)，远程图像文件用 `/attach <path>`。OpenSSH loopback X11 显示自动检测。对显式转发的 Wayland 或非 loopback X11 显示，设置 `CODEWHALE_SSH_CLIPBOARD=graphical`；设为 `terminal` 强制终端传输而不是环境远程显示。DeepSeek 的公共 Chat Completions API 目前接受文本消息内容，所以媒体附件作为显式本地路径引用发送，而不是原生图像/视频负载。附件行在提交前出现在输入区上方；移到输入区开头，按 `↑` 选择附件行，然后按 `Backspace` 或 `Delete` 移除它，不用手工编辑示例文本。

## 受管配置与需求(Managed Configuration and Requirements)

codewhale 支持策略分层模型：

1. 用户配置 + profile + 环境覆盖
2. 受管配置(若存在)
3. 需求验证(若存在)

Unix 上默认：
- 受管配置：`/etc/deepseek/managed_config.toml`
- 需求：`/etc/deepseek/requirements.toml`

需求文件形态：

```toml
allowed_approval_policies = ["on-request", "untrusted", "never"]
allowed_sandbox_modes = ["read-only", "workspace-write"]
```

如果配置值违反需求，启动以描述性错误失败。

## 关于 `codewhale doctor` 的说明

`codewhale doctor` 遵循与 TUI 其余部分相同的配置解析规则。这意味着 `--config`、`CODEWHALE_CONFIG_PATH` 和旧 `DEEPSEEK_CONFIG_PATH` 都被尊重，MCP/skills 检查使用解析出的 `mcp_config_path` / `skills_dir`(含环境覆盖)。

要引导缺失的 MCP/skills 路径，运行 `codewhale setup --all`。你也可以运行 `codewhale setup --skills --local` 创建自有的项目目录 `.codewhale/skills`。项目技能在工作区获得信任后加载。

普通 `codewhale doctor` 和 `doctor --json` 都是结构性的，默认离线。它们不检查 release 服务、托管 provider API、本地 provider 端点或 MCP 进程，也不加载工作区凭据 `.env`。用 `--check-updates`、`--probe-api`、`--probe-local` 或 `--probe-mcp` 选择加入对应的实时边界；`--probe-local` 可能启动 Ollama 这样的桌面托管服务。只有显式 API/local 探测路径可以加载工作区凭据 `.env` 值。实时标志与 `--json` 冲突，所以机器可读 doctor 输出总是离线。顶层键包括 `version`、`paths`、`secret_backend`、`config_path`、`config_present`、`workspace`、`api_key.source`、`api_key.availability`、`base_url`、`default_text_model`、`mcp`、`skills`、`tools`、`plugins`、`sandbox`、`platform`、`api_connectivity` 和 `capability`。CI 消费者应依赖 `api_key.source`(`config_declared`/`env_declared`/`external_auth_declared`/`secret_store_unprobed`/`secret_store_unavailable`/`oauth_unprobed`/`external_consent`/`none`/`local_runtime`/`unknown`)和 `api_key.availability`(`present`/`not_required`/`not_probed`/`unavailable`/`unknown`)，而不是解析人类可读的 `doctor` 文本。Source 是声明元数据，不是凭据存在或工作的证明。只有非空、非哨兵的字面配置值才在结构上 `present`；无认证和本地路由是 `not_required`。环境、外部认证、OAuth、consent 和 secret-store 声明保持 `not_probed`，不能让结构 Setup 或 Fleet 就绪成为真。被禁止使用共享存储的命名/自定义端点上的 secret-store 哨兵是 `secret_store_unavailable`/`unavailable`，而 `unknown` 保留给缺乏受支持结构结论的情况。确切和空白包裹的旧哨兵从不被当作字面凭据。结构加载器仍遵循安全环境路由/模型/策略字段，但从不物化环境 HTTP 头、沙箱 API key 或搜索 API key；只有显式 API/local 探测切换到正常凭据加载路径。选择加入的更新检查也只发出类型化通用失败：不可信 release 元数据和传输错误不被回显。

如果配置加载或验证失败，`doctor --json` 返回非零，并打印带 `status = "error"` 和 `error.kind = "config_validation"` 的有界 JSON 错误信封。它对无效配置不发出正常路由或能力报告——也不发出可能敏感的底层错误。

除非运行显式 MCP 命令，MCP 条目是配置诊断。`mcp.probe_scope` 是 `configuration`，`mcp.live_health_checked` 为 false，每个服务器把 `checks.configuration` / `checks.command` 与 `checks.process_reachable`、`checks.protocol_initialized` 和 `checks.backend_tool_health` 分开。后三者在 doctor 输出中保持 `not_checked`。运行 `codewhale mcp validate` 显式启动启用的服务器并验证协议初始化/发现；后端健康仍需适当的显式工具调用。Doctor 只报告安全的结构性 MCP 字段：URL userinfo/path/query/fragment、原始命令参数、环境值、头值和 token 材料被省略。Provider URL 遵循同样规则，只暴露 `scheme://host[:explicit-port]`。

`capability` 键包含从静态知识(release 文档、API 指南)派生的按 provider 能力信息，而不是实时 API 探测。顶层子键：`resolved_provider`、`resolved_model`、`context_window`、`max_output`、`thinking_supported`、`cache_telemetry_supported` 和 `request_payload_mode`。

在 CI 脚本中用 `capability.context_window` 和 `capability.max_output` 做模型上限检查；不要把 `capability.max_output` 当作每回合请求预算。用 `capability.thinking_supported` 决定是否配置思考强度。

## Setup 状态、清理与扩展目录

`codewhale setup` 接受除现有 `--mcp`、`--skills`、`--local`、`--all` 和 `--force` 之外的几个标志：

- `--status`——打印紧凑的单屏状态(api key、base URL、模型、MCP/skills/tools/plugins 计数、沙箱、`.env` 存在)。只读且无网络；在 CI 中安全。如果工作区中 `.env` 缺失而 `.env.example` 存在，状态输出指向 `cp .env.example .env`。
- `--tools`——用描述自描述 frontmatter 约定(`# name:` / `# description:` / `# usage:`)的 `README.md` 和一个遵循它的 `example.sh`，搭建 `~/.codewhale/tools/`。该目录刻意不自动加载；通过 MCP、hooks 或 skills 把单个脚本接入智能体。
- `--plugins`——用 `README.md` 和 `example/plugin.toml` 加一个命名空间示例 Skill 搭建 `~/.codewhale/plugins/`。包被只读、不可信、禁用地发现；启用前通过 `/plugin` 审查。v0.9.1 只激活声明的 Skills 和 MCP 服务器。见 [PLUGIN_BUNDLES.md](./PLUGIN_BUNDLES.md)。
- `--all` 现在一起搭建 MCP + skills + tools + plugins。
- `--clean`——列出 `~/.codewhale/sessions/checkpoints/latest.json` 和 `offline_queue.json`(若存在)。旧 `~/.deepseek/sessions/checkpoints/` 文件不自动扫描；一次性旧版清理设置 `CODEWHALE_HOME=~/.deepseek`。传 `--force` 才实际移除匹配文件。这从不触碰真实会话历史或任务队列。

`--status` 和 `--clean` 与搭建标志互斥。

## 为什么引擎剥离 XML/`[TOOL_CALL]` 文本

codewhale 只通过 API 工具通道(结构化 `tool_use` / `tool_call` 项)发送和接收工具调用。`crates/engine/src/core/engine.rs` 中的流循环识别一组固定的假包装开始标记——`[TOOL_CALL]`、`<codewhale:tool_call`、`<tool_call`、`<invoke `、`<function_calls>`——并把它们从可见助手文本中擦除，从不让它们变成结构化工具调用。包装被剥离时，循环每回合发出一条紧凑 `status` 通知，让用户看到可见文本为何缩小。把任何重新启用基于文本工具执行的更改视为回归；`crates/tui/tests/integration/protocol_recovery.rs` 中的协议恢复测试锁定该契约。

## 面向模型的脱敏（`[redaction] model_bound`）

Codewhale 会在工具输出**发送给上游模型之前**，把形似凭据的值遮蔽掉——这就是“模型边界”。工具读到的文件里可能含有已配置的 API key、裸的 provider token，或形似凭据的不透明字符串，而模型不该看到这些字节。这一兜底与显示/导出用的擦除器是分开的：它决定模型自己能引用回来的内容，并且刻意保持保守（`CredentialShaped` 策略，见 `crates/config/src/persistence.rs`），所以普通的代码和配置保持逐字节不变，而密钥、JWT、bearer token、PEM 块和长的不透明字符串会被遮蔽。

把这种遮蔽**关掉**是一项安全决定，所以它不是一个普通的布尔值：

```toml
[redaction]
model_bound = "disabled"   # "enabled" (default) | "disabled"
```

设置为 `"disabled"` 只是记录一个*请求*。只有当以下条件全部满足时它才会生效：

1. 你重启交互式 TUI。
2. 启动门出现，你在它的第一阶段（“confirm and disable”）按 `1`/`Y`。这只会前进到第二个、最终确认的阶段——该门会重复红色警告，并询问“are you really sure?”。
3. 在第二阶段，你再次按 `1`/`Y`。该门采用与工作区信任相同的显式按键规则渲染——`Enter` 绝不会凭反射确认，第二阶段的 `2`/`U` 会退回一步。
4. 只有第二次确认才会把一份回执持久化到 `~/.codewhale/redaction-state.json`（与 `config.toml` 相邻），并在本次启动及以后的启动中，用关闭遮蔽的方式重建引擎。

回执与它所依据的那份配置绑定，只有当该配置仍然请求 `"disabled"` 时才有效。把 `model_bound` 改回 `"enabled"`——或者在确认之后以任何方式重写 `config.toml`——都会使它失效，所以之后再次请求 `"disabled"` 时，下次启动会要求重新确认。回执会同时检查配置内容和修改时间；配置缺失、不可读或格式错误，以及没有这种绑定的旧回执，都会让遮蔽保持启用。使用旧版 home 的安装会把回执存放在其解析出的配置文件旁边。

在确认存在之前，有效模式始终是 `"enabled"`：

- 选择 `2`/`U`（“keep masking on”）不会改动配置字段，所以下次启动会再次询问。把该字段改回 `"enabled"` 就不会再被询问。
- 非交互入口（`codewhale exec`、hooks、自动化、无头智能体）从不确认任何事，也从不应用未经确认的请求。
- 路由/分类摘要和持久的目标状态文本，无论该开关如何，都保持它们自己始终开启的脱敏；这个选择退出的存在，是为了让模型能引用文件字节以进行精确编辑，而不是为了放宽已存储的状态。

配置值本身是宽容的：`true`/`false`、`"on"`/`"off"` 和 `"enabled"`/`"disabled"`（任意大小写）都能解析，其中 `false`/`"off"` 表示 `"disabled"`。

已确认的选择退出仍会把你配置的 API key 发送给你正在交谈的那个 provider。仅当模型必须读取并编辑含有真实凭据的文件时才使用它。

### 已存储的会话

遮蔽在工具输出进入转录时执行一次，所以已保存的会话文件（`~/.codewhale/sessions/*.json` 及其检查点）和 Runtime API 的线程项（它们的文本和工具元数据）不会存储工具输出中形似凭据的值。启用上面已确认的选择退出后，工具输出会按模型所见的样子存储。

尚未遮蔽的有：溢出到 `~/.codewhale/tool_outputs` 的大输出、shell 完成证据产物，以及工具调用的*输入*（例如 `curl -H "Authorization: Bearer …"` 命令行）。`doctor` 和 `scrub-secrets` 同样不检查这些。

在这一更改之前的构建所保存的会话，其工具输出里可能仍含有凭据；没有任何东西会自动改写它们。`codewhale doctor` 会在 **Stored Sessions** 下报告它们（它检查最新的 50 个文件），而下面这条命令会在每个已保存的会话中找到并遮蔽它们：

```sh
codewhale sessions scrub-secrets          # report only
codewhale sessions scrub-secrets --apply  # rewrite the affected files
```

`--apply` 会在保存所用的同一个逐会话锁下重写每个文件，所以它绝不会丢失并发的保存。仍处于打开状态的会话可能在下次保存时把它内存中的副本写回去，因此请先关闭打开的会话，并轮换任何曾经暴露的凭据——遮蔽一份已存储的副本并不能让泄露收回。
