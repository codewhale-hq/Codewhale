# Auto-Review

> 英文原文：[AUTO_REVIEW.md](../AUTO_REVIEW.md)。
> 最后与英文同步日期（last synced with English revision）：2026-11-24。

Auto-Review 是一种权限设置：Codewhale 自己审查每一次工具调用，而不是来问你。安全的调用直接运行，危险的调用被阻断并给出原因，拿不准的调用交给第二个模型单独判断这一次调用。只有少数确实需要人来决定的事才会问你。

它是[模式与权限姿态](MODES.md#权限姿态)中所述权限拨盘的一档，也是[授权顺序](AUTHORIZATION_ORDER.md)中的一层。它不会绕过工具准入、仓库保护规则或[沙箱](SANDBOX.md)。

## 开启

- 反复按 `Shift+Tab`，直到页脚显示 Auto-Review，或运行 `/auto`。
- 也可以运行 `/config`，把 `permission_posture` 行设为 `auto-review`。
- 从 shell 启动时加 `--permission-mode auto-review`。

内部值是 `auto`；配置中的 `approval_policy = "auto"` 同样会选中它。

## 一次调用如何被判定

两层来判定。第一层是固定规则，第二层是模型。

### 第一层：确定性底线

底线由你配置的阻断规则加上内置安全底线组成。它从不经过模型审查，在无人值守的工作中也不会等待审批卡片。它对一次调用做三件事之一。

**放行**它能够证明安全的调用。这包括明确属于例行范围的工作区测试与构建命令，也包括带末尾 `2>&1` 或 `>/dev/null` 以及单个 `head`、`tail` 或字面 `grep` 过滤的同类命令。不包括 `cargo run`、非例行的 `make` 目标、`git add`、`git commit`、丢弃改动的标志，以及你自己的阻断规则点名的任何东西。

**直接阻断**：

- 包与发行版发布：`npm publish`、`cargo publish`、`gh release` 及发布工具。
- 无人值守运行中的破坏性操作（见[有人值守与无人值守的运行](#有人值守与无人值守的运行)）。
- 无人值守运行中的任何仓库推送或标签变更。

**拦下**交由模型守护者，且优先于你的 `auto_review.allow` 规则：

- 交互式会话中向仓库远端推送。
- 命名的软规则：`force-push`、`git-tag`、`deploy-branch`、`git-discard`、`infra-destroy`、`ci-or-pr-bypass`、`print-credentials`、`secret-commit` 和 `deploy-wrapper`。其中 `print-credentials` 规则包括 `.claude.json` 和 `.config/gh/hosts.yml`。
- 针对 `AGENTS.md`、`CLAUDE.md` 或已加载指令文件的写入、编辑、删除或移动。
- 针对受保护的仓库、凭据、IDE 或智能体、工作流、shell 启动或自启动路径的文件或 shell 写入：`.git`、`.codewhale/`、`.vscode/`、`.claude/`、`.mcp.json`、`.claude.json`、`.github/workflows/`、`.gitconfig`、`.config/gh/hosts.yml`、shell rc 与 profile 文件、`Library/LaunchAgents/`、`.config/autostart/`、`.cargo/config.toml`、`.config/fish/`、`.config/systemd/user/`，以及 Windows 的 `Startup` 文件夹，包括符号链接与路径别名指向的目标。这些同样优先于窄精确放行规则与例行检查。
- 出站网络请求，以及底线无法证明安全的任何调用。

还有三条规则属于底线：

- **没有 git 的文件夹。** 只有当启用本地快照（`snapshots_enabled`）且工作区根目录不是 `/`、主文件夹或顶层文件夹时，有界的工作区写入才被视为安全。在这类文件夹中删除文件或清空整个文件仍会交给守护者。git 工作树的 Accept Edits 规则不变。
- **工作区之外的首次读取。** 首次解析到工作区之外的 shell 读取，在有可提示的交互式所有者时每次 Auto-Review 会话询问你一次。`/dev/null` 这类标准伪设备不计入。无人值守的运行则改为交给守护者。当你离开 Auto-Review、回退或重置会话时，这个一次性的会话状态会重置。
- **关键路径删除。** 删除文件系统根目录、主文件夹、项目根目录或其任意上层目录，或 `.git` 文件夹，在交互式会话中会询问你，在无人值守或无头会话中则直接拒绝。

### 第二层：模型守护者

被拦下的调用交给一次性的守护者模型。它返回风险级别、允许或拒绝，以及理由。

- **用哪个模型。** 设置了 `auto_review.reviewer_model` 时用它。否则在会话当前启用的提供商路由上选择有能力的模型（若匹配），再否则用会话模型。它从不切换提供商或账户。
- **它有什么。** 没有工具，也不记此前的规则。它发出一次请求，在超时或传输失败后重试一次，两者都在 90 秒预算之内。仍然无法完成的审查（超时、传输失败、输出格式错误或不完整，或上下文超出预算）属于审查失败，而不是安全拒绝：在有所有者在场、且所有者可被提示的回合（不是子智能体 worker）中，它会为这次确切的调用弹出正常的审批卡片，且不计为一次阻断或一次不可用检查；否则该调用以失败关闭（fail closed）的方式被拒绝，并给出审查失败的消息。取消总是失败关闭。
- **高风险或严重风险**不能自动运行，除非你批准对这次确切调用的一次性重试。即使有这次批准，守护者明确的拒绝仍会阻断。
- **软规则拦截**会在拦截、阻断原因和回执中写出规则名称。它们需要你对该操作提出明确请求，而不是一句宽泛的指示。

守护者只收到标注为不受信任证据的结构化内容，别的都没有：

| 它收到 | 说明 |
| --- | --- |
| 确切的 `proposed_tool_call`，包括其 `description` | 从不截断。 |
| 确定性观察 | 包括 `person_approved_retry` 与任何触发的 `soft_rule`。 |
| `recent_actions` | 当前回合此前经脱敏的非只读动作。 |
| `approved_plan` | 有已批准计划时。 |
| `environment` | 来自用户或受管配置的范围条目。项目级 `.codewhale/config.toml` 不能设置或修改 `auto_review.environment`。 |
| `project_instructions` | 在会话开始时捕获，用户级与仓库级分开标注并附 SHA-256 哈希；会话期间不再从磁盘重读。 |
| `person_messages` | 对话所有者输入的内容，最新在前。在子智能体回合中，这些来自根对话，而不是子智能体的简报。由智能体创建的后台线程中为空。在运行时线程上，所有者输入框是已登录的 web 客户端，或在回合或引导请求中声明 `"client": "desktop"`（或 `tui`／`web`）的 runtime-token 调用方；其他 Runtime API 调用方不记录任何内容。 |

它从不收到附带的 `@file` 正文、技能正文、展开的上下文、工具输出或助手散文。

上下文上限为 64 KiB。当前请求与确切提议的调用从不截断；如果它们本身就超出上限，审查以不可用的方式失败关闭。预算紧张时，先裁剪仓库指令，再裁剪用户指令，然后裁剪中间最旧的 `person_messages`，同时保留最早输入的限值与最新的请求。

## 调用被阻断时

每次阻断都会点名规则与原因。随后 Codewhale 会改走别的路径，或者在有你在场的交互式会话中，提供这次确切调用的一次性批准：

- 在阻断回执上、于空输入框中按 `r`，
- 运行 `/permissions blocked`，或
- 调用 Runtime API：`POST /v1/threads/{id}/auto-review/approve`。

重试授权绑定到确切的 `(tool_name, call_digest)`，用一次重试就耗尽，并且仍然要经过所有其他关卡：钩子、带类型的权限、仓库规则、确定性底线和沙箱。它从不延伸到子智能体或扩展调用，并会在你离开 Auto-Review 或重置对话时清除。

## 反复阻断

| 运行 | 阈值 | 结果 |
| --- | --- | --- |
| 你在场，且这不是子智能体 worker | 连续 3 次阻断、对话中累计 20 次阻断，或连续 3 次审查不可用 | 权限切换为 Ask（`AutoReviewPaused`），工作继续在 Ask 下进行。切回 Auto-Review 会重置计数。 |
| 无人值守 | 累计 20 次阻断，或连续 10 次审查不可用 | 运行绝不回退到 Ask。它会越过阻断继续，并以“已中断”结束，附上被阻断动作的摘要。 |

一次放行的审查会重置连续阻断与不可用计数。守护者明确的拒绝会重置连续不可用计数。

## 有人值守与无人值守的运行

每个回合都带有引擎铸造的两个事实。宿主或请求字段不能提供其中任何一个。

- **运行来源。** 只有当回合由所有者活动的终端输入框直接发起或引导，或由已附着所有者界面的、已登录的所有者运行时输入框发起或引导时，才是 `Interactive`。`exec` 与 ACP 运行（包括 `approval_policy = "auto"`）为 `Headless`。分离启动、分离式子智能体、任务、自动化与延迟的 `send_later` 运行为 `Background`。
- **所有者在场**（`owner_present`）。

本页中的“无人值守”指 `Background`、`Headless`，或任何 `owner_present` 为假的回合。

**你在场时。** 由别的东西强制产生的提示会弹出正常的审批卡片，而不是被跳过或失败关闭：带类型的 `permissions.toml` `ask` 规则、回答 `ask` 的 `tool_call_before` 钩子、Computer Use 同意与脚本卡片这类仅限提示的工具，以及需要人参与的仓库规则拦截。例外是硬阻断（`hidden_unicode` 或配置的 `auto_review.block` 规则）。在运行时线程上，只有所有者的界面（`desktop`、`tui` 或 `web`）可以回答这类卡片。

**你不在场时。** 绝不弹出审批卡片。强制提示会点名该规则被拒绝，且不登记待决审批；需要人参与的仓库规则拦截则直接阻断。

**无头 `exec`** 对非破坏性的拦截咨询模型守护者，对破坏性工作应用确定性的无人值守规则。

**ACP** 回合只使用确定性规则，并拒绝只有守护者才能决定的调用。见 [Runtime API：ACP stdio 适配器](RUNTIME_API.md#acp-stdio-适配器codewhale-serve---acp)。

**扩展调用**被拦下时会带着宿主铸造的插件身份交给守护者。它们绝不会被例行档位或允许规则清除。独立的带类型规则、钩子拦截与安全底线阻断仍然生效。

## 向你提问

有意的提问在交互式会话中通过 `request_user_input` 保留。一个问题会挂起回合，直到你回答或取消，或配置的超时到期。无头 `exec` 不提供该工具，因为没有人能回答。工具安全拦截与这些提问是分开的。

## 子智能体与 Fleet worker

Auto-Review 父级下的 worker 遵循同一策略：

- 已证明安全的调用会运行。类发布与破坏性的后台工作被阻断。
- 它无法证明安全的拦截交给同一个一次性守护者，使用该子智能体自己的会话客户端。
- 绝不为子智能体弹出提示。守护者不可用时拒绝。

当一个子智能体在运行过一次或多次工具调用后回到 Auto-Review 父级时，守护者会检查一次子智能体的已执行动作列表。它只看到工具名称以及经压缩、脱敏的调用入参，绝不看工具结果或子智能体的报告正文，并将它们与根所有者的消息（`person_messages`）、已批准的计划和固定的项目指令相对照。如果它标记出疑虑，或无法运行（不可用、超时、取消或超出上下文预算），引擎会在交付给父级的报告顶部放置一条经过清洗的 `[Auto-Review warning: ...]` 横幅。

来自子调用的阻断与来自返回审查的阻断，都会以 `[sub-agent <id>]` 条目的形式记录在父级的已阻断动作列表中（`/permissions blocked` 与 `GET /v1/threads/{id}/auto-review/blocks`）。它们不能获批重试（`approvable: false`）。

子智能体在其他权限设置下的行为见[模式与权限姿态](MODES.md#子智能体sub-agents-与-fleet-worker)。

## 配置

`[auto_review]` 表（allow 与 block 规则、`reviewer_model`、`environment`、`review_all_shell`，以及受管的 `disabled` 开关）记录在[配置](CONFIGURATION.md)中。项目配置文件不能设置它。

## 设计参考

LLM 守护者最接近 OpenAI Codex 在提交 [`6fc6b9d6d2580d62622fc9884b5f5707f6505a5e`](https://github.com/openai/codex/tree/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e) 中的实验性 Auto-Review。Codex 的[守护者入口点](https://github.com/openai/codex/blob/6fc6b9d6d2580d62622fc9884b5f5707f6505a5e/codex-rs/core/src/guardian/mod.rs)会重建完整对话记录并运行一个专门的审查会话。Codewhale 只传入 Rust 记录的所有者 `person_messages`、可选的已批准计划文本、经脱敏的此前非只读动作、会话固定的指令、确定性观察，以及在 64 KiB 上限内的确切 `proposed_tool_call`，并配套 90 秒截止时间、失败关闭的结果与按回合的拒绝熔断。它排除助手散文与工具输出，也不复制 Codex 的对话记录重建、用户授权评分、守护者工具、重试、持久审查会话或拒绝台账。

Kimi Code 在提交 [`1414d4602898f406e540b23342cb18db23ff9efc`](https://github.com/MoonshotAI/kimi-code/tree/1414d4602898f406e540b23342cb18db23ff9efc) 中同样没有 LLM 守护者。它按序的[权限策略](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/permissionPolicyService.ts)先应用显式拒绝规则，然后由其 [Auto 策略](https://github.com/MoonshotAI/kimi-code/blob/1414d4602898f406e540b23342cb18db23ff9efc/packages/agent-core-v2/src/agent/permissionPolicy/policies/auto-mode-approve.ts)直接返回 `approve`。Codewhale 使用上文所述的确定性底线与守护者，同时保留有意的用户提问。

沙箱与升级基线以 DeepSeek Harness `0.1.0-rc.5`（提交 [`47f943859bef60e4160492346772ded9b24f765a`](https://github.com/deepseek-ai/deepseek-harness/tree/47f943859bef60e4160492346772ded9b24f765a)）为基础：它的[沙箱契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/sandbox.md)按调用定义 `read-only`、`workspace-write` 与 `danger-full-access` 边界，并禁止静默的无约束回退；它的[审批契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/approval.md)只授予 `allowed-once`，并在拒绝、取消或应答方不可用时失败关闭；它的[沙箱结果契约](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/shell/bash-sandbox/README.md)要求模型在被拒绝的命令上，只以最窄的更宽模式加一条理由重试一次。DeepSeek Harness 没有在这条路径上加入 LLM 守护者。Codewhale 的自主姿态只增加上文所述的那一次无状态守护者请求；确定性的硬阻断在会审查的姿态中仍然不可绕过。
