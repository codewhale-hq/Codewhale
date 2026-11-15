# 授权顺序

> 英文原文：[AUTHORIZATION_ORDER.md](../AUTHORIZATION_ORDER.md)。
> 最后与英文同步日期（last synced with English revision）：2026-11-15。

Codewhale 把工具（tool）可用性、钩子（hook）、带类型的权限规则、审批姿态（posture）、
仓库保护规则与沙箱（sandbox）组合在一起。某一层给出的审批（approval）并不是通用的
绕过手段：后面的安全层仍然可以要求复审或阻断这次调用，而一次审批也不等于
操作系统层面的沙箱授权。

本页记录的是交互式引擎处理“由模型请求的工具调用”这条路径所实现的顺序。
只用到这条路径一部分的入口——例如核心运行时的直接工具 API——会保持它们
实际用到的那几层之间的相对顺序。

## 模型工具调用流水线

交互式引擎按下面的顺序评估一次由模型请求的工具调用：

| 顺序 | 层 | 结果 |
|---:|---|---|
| 1 | 生效的配置与姿态 | 用户设置、命令／运行时（runtime）覆盖项，以及项目覆盖层都在回合（turn）开始之前完成解析。项目覆盖层可以收紧 `approval_policy`、`sandbox_mode` 或 shell 可用性，但不能放松它们。 |
| 2 | 模式与工具准入 | Plan 模式的限制、输入解析错误、按命令的拒绝／允许工具列表、调用方限制，以及缺失的执行注册，都在考虑策略规则之前先失败。一个工具如果同时出现在两个命令列表里，就会被拒绝。 |
| 3 | 准备阶段，然后是 `tool_call_before` 钩子 | 注册表（registry）的准备阶段不产生副作用（包括 `plugin_manager` 的只读目标解析与范围判定，审批前绝不发起任何网络拉取）。随后前台钩子按 `deny > ask > allow` 折叠，而一个严格匹配却不产出裁决的钩子会失败关闭（fail closed）。最后一个 `updatedInput` 生效；在后面的关卡检查之前，引擎会再次准备这份改写后的输入。 |
| 4 | 已注册工具的基线 | 已准备工具自身的 `ApprovalRequirement` 确立了它通常的审批需求。钩子的 `ask` 在完成该赋值之后才施加，所以基线无法把它抹掉。不可绕过的已注册拦截在能够弹提示的姿态下依然强制执行；Full Access（完全访问）则会自动批准它们，而不是打开一个自相矛盾的模态框。Plan 模式在这里也会阻断具备写入能力的工具。 |
| 5 | 带类型的 `permissions.toml` 规则 | 匹配上的 `deny` 会阻断。在 Ask 与 Accept Edits 中，匹配上的 `allow` 可清除注册表层面的普通审批（但不能清除钩子 `ask` 或不可绕过的已注册拦截）。在 Auto-Review 下，宽泛的带类型 `allow` 规则（不带 `command` 的 `exec_shell`/`bash`、`*`/`python*`/`npm run *` 等通配命令、`python`/`node`/`npm run`/`cargo run` 等宽泛运行器前缀、`agent`/`subagent`/`spawn_agent`、`*`，以及不带 `path` 的写文件/编辑/补丁规则）会在不修改 `permissions.toml` 的前提下暂停，并在离开 Auto-Review 时自动恢复；只有窄范围带类型 `allow`（所有静态展开片段都不含动态替换、重定向、控制符、shell/启动器包装或非完全匹配路径限定、仅带无标志参数或精确 `command_exact` 的 `exec_shell`/`bash`；以及不涉及智能体/扩展、文件工具带显式 `path` 的精确工具名）在未命中安全底线、软规则、钩子/扩展拦截或 `auto_review.review_all_shell` 时跳过 Auto-Review 审查模型。匹配上的 `ask` 在能够弹提示的姿态下强制复审，包括 `owner_present` 为真时的 Auto-Review（在运行时线程上只有所有者界面可以作答），而在 `owner_present` 为假或 Never 姿态下则直接点名该规则拒绝。Full Access／自动批准不会被降级成提示，而显式的带类型 `deny` 依然阻断。 |
| 6 | Auto-Review 策略与内置安全底线 | 引擎为每个回合铸造运行来源（所有者在场的 `Interactive`，对比 `Headless`、`Background` 或无人值守；请求或宿主字段均不可指定）。评估顺序为：配置的阻断规则先于内置底线，随后检查命名软规则、扩展拦截、`auto_review.review_all_shell`（开启时）、有效窄带类型 `allow`、配置的窄范围 `auto_review.allow` 规则以及确定性兜底。宽泛的 `[[auto_review.allow]]`（`action_kind` 归一化为 `shell`/`external`/`write`/`destructive`/`publish` 且未限定非 shell/非智能体工具，或 `tool` 为 `exec_shell`/`bash`/`agent`/`*`）在 Auto-Review 中会被带警告忽略，仅保留具体的非 shell/非智能体工具允许规则。用户/受管配置中的 `auto_review.review_all_shell = true` 会将每条 shell 命令（包括普通只读命令与窄带类型允许命令，在底线、阻断、软规则和扩展拦截之后）送往 Auto-Review 审查模型。这一层在带类型权限之后运行，可以追加一次提示或一次阻断，但无法移除更早的拦截。内置底线拒绝含有隐藏 Unicode 字符（`hidden_unicode`）的 shell 命令、shell 输入或校验器，并在无提示的情况下拦住类似发布的动作、破坏性的无人值守／后台／无头动作以及 `curl`/`wget` 凭据上传（在 Auto-Review 与 Never 中直接阻断，绝不等待审批卡片）。在 Auto-Review 中，当 `owner_present` 为真且未命中硬阻断（`hidden_unicode` 或配置的 `block` 规则）时，非扩展自身的更早强制提示（带类型 `permissions.toml` 的 `ask` 规则、`tool_call_before` 钩子的 `ask`、仅限提示的工具如 Computer Use 同意／脚本卡片）会弹出正常审批卡片，而不再咨询守护者或失败关闭；当 `owner_present` 为假时，强制提示直接拒绝且不登记待决审批。需要审批的扩展动作（`core/call`、宿主服务操作与插件宿主工具调用）会带着宿主铸造的插件身份交由 Auto-Review 审查模型结合上下文审查，而不直接拒绝或跳过，也无法通过例行放行、`allow` 规则或一次性重试授权绕过（独立的带类型规则、钩子拦截与安全底线阻断依然生效）。对于合乎条件的非子智能体、非扩展调用，所有者在场时可为单个被 Auto-Review 阻断的精确调用授予一次性重试授权（绑定 `(tool_name, call_digest)` 且仍必须通过前后所有非 Auto-Review 关卡）。所有者在场时若连续阻断 3 次、会话累计阻断 20 次或连续 3 次审查不可用，Auto-Review 会切换为 Ask 并在 Ask 下继续当前批次；无人值守运行绝不回退到 Ask，越过阻断继续并在累计 20 次阻断或连续 10 次审查不可用时以已中断结束。无头 `exec` 在非底线回退拦截上仍可咨询模型守护者，而 ACP 只使用确定性规则。Full Access 跳过内置底线。 |
| 7 | 仓库保护规则（repository law） | 受保护路径的不变式只能追加一次提示或一次阻断。仓库规则给出的提示在 Ask 系列姿态和所有者在场的交互式 Auto-Review 中会弹出审批卡片，而在 Full Access、Never 以及无人值守／无头 Auto-Review 下会变成硬阻断（因为这些情形下不存在可回答的审批模态框）。 |
| 8 | 人工审批 | 剩下的提示会被送进审批通道。拒绝会终止这次调用。批准只授权这次已规划好的调用；会话（session）级与持久化的选择会影响之后匹配的调用，但不会抹掉这次调用后面的关卡。 |
| 9 | 工具权限与执行沙箱 | 执行期间，worker 权限信封、原生工具路径检查，以及所选的操作系统级或外部沙箱仍然生效。沙箱拒绝就是拒绝，除非用户另行授权一条受支持的提权路径。 |

在带类型权限层之后，这个顺序是刻意保持单调的：Auto-Review 和仓库保护规则可以让结果更严，
但不能把先前的阻断或提示变成未经复审的执行。这些层内部存在显式的姿态选择——
比如 Full Access 不会创建交互式发布提示——但这些选择不允许更早记住的授权抹掉
该层实际产生的拦截。

## 带类型权限规则的选择

`permissions.toml` 目前与生效的用户 `config.toml` 位于同一目录。今天没有项目本地的
权限规则来源。可选的 `workspace` 字段把某一条用户规则限定到某个仓库；它不会创建
项目覆盖层。`/permissions` 会报告该来源、匹配器、作用域，以及该作用域是否适用于
当前工作区。

执行策略引擎按下面的顺序评估匹配的规则：

1. 归一化工具、命令、工作区，以及任何工作区相对路径。不安全或外部的路径不会变成
   可匹配的文件规则。
2. 把被拒绝的命令前缀与整条命令以及每个链式片段比对。这些硬前缀拒绝会跨规则集合并，
   并且始终胜出。
3. 为未链式的 shell 命令计算一个可信前缀候选。这只是审批模式兜底的候选，
   不是当即的结论。
4. 按下面的字典序优先级选出一条匹配的带类型规则：
   1. 更高的来源层：`User > Agent > BuiltinDefault`；
   2. 同一层内更强的动作：`deny > ask > allow`；
   3. 当层与动作相同时，匹配器更具体的那条。
5. 施加选中的带类型动作。`deny` 禁止这次调用，`allow` 跳过执行策略审批，
   `ask` 要求审批。带类型的 `ask` 覆盖可信前缀。
6. 如果没有带类型动作来决定结果，就用可信前缀候选套用审批模式兜底。

来源层的比较先于带类型动作。因此，用户层的带类型 `allow` 可以覆盖 `Agent` 层的带类型
`deny`；而在同一层内，`deny` 依然胜过 `ask`，`ask` 胜过 `allow`，与文件顺序或
匹配器具体程度无关。硬拒绝前缀是例外：它们在带类型层选择之前就被检查，
无法被带类型的 allow 覆盖。

具体程度只是在来源和动作之后的第三顺位。受限命令、精确命令、路径或工作区匹配器，
胜过同一层里动作相同的工具级规则。具体程度永远不会让一条窄范围的 allow 打败
同层的 deny。

对于链式 shell 命令，可信前缀永远不会批准整条链。任何一个片段胜出的带类型 deny
都会阻断整次调用。

## 审批姿态与缺失的提示

执行策略的结果与已注册工具的基线是合并在一起的；两者不应被独立解读。

- Ask 和 Auto-Review 可能弹出工具安全审批。在 `owner_present` 为真的 Auto-Review
  中，强制提示（带类型 `permissions.toml` 的 `ask` 规则、`tool_call_before`
  钩子的 `ask`、Computer Use 同意／脚本卡片等仅限提示工具、要求人工参与的仓库规则拦截，
  以及强制提示的创作者调用）在未命中硬阻断（`hidden_unicode` 或配置的
  `auto_review.block` 规则）时会弹出正常审批卡片；在运行时线程上，只有所有者界面可以回答该卡片。在
  Auto-Review 下需要审批的扩展调用会带着宿主铸造的插件身份路由至 Auto-Review
  审查模型，且绝不会通过例行放行或 `allow`
  规则豁免（若同时命中带类型规则或钩子的强制提示，则仍遵循相同的交互式/无人值守规则）。无人值守与无头
  运行（`Background`、`Headless` 或 `owner_present` 为假）会点名该规则直接拒绝强制提示，
  且不登记待决审批；非强制的 Auto-Review 回退拦截则继续经由确定性规则与模型守护者处理而不弹提示。
  由模型编写的用户提问（`request_user_input`）是另一条通道，在任何交互式姿态下都能到达用户，
  Auto-Review 也不例外。只有没有应答方的无头 `exec` 运行会收起这个工具。
  运行时线程（app 对话、后台任务与自动化）保留它：一个问题会挂起整个回合，
  直到有人通过 app 或运行时 API 作答或取消它，或者为正数的
  `tools.user_input_timeout_seconds` 到期。省略或为零的超时时间会无限等待，
  与 Ask 姿态下审批挂起的方式相同。
- 创作者 `plugin_manager` 的只读查询（`list_plugins`、`list_bundles`、`list_version_exemptions`）
  在各姿态下均无需提示直接执行。变更类调用（`install_bundle`、`remove_bundle`、`set_bundle`、`set_plugin`）
  必须发生在交互式顶层直接调用中（`Plan`、`Never`、`Background`、子智能体以及嵌套的 `execute_tools`
  都会以 `PermissionDenied` 拒绝）：工作区作用域内的本地变更在 `Ask`／`Accept Edits` 中弹出人工卡片、
  在 `Auto-Review` 中经由守护者（或配置的 `auto_review.allow` 规则）审查、在 `Full Access`
  下运行（并将智能体姿态记入 `plugins/state.json`）；而作用域外目标（项目外路径、远端源或含未解析 npm
  依赖的本地目录、由人批准过的插件以及内置开关）会跳过 Auto-Review 守护者，即使在配置了 `Auto-Review`
  allow 规则或 `Full Access` 下也强制弹出 `creator-human:` 人工卡片。记忆的非用户授权、钩子和带类型
  `allow` 规则均不能豁免该卡片；未见远端或含 npm 依赖的拉取在收到 `"yes"` 前绝不执行，且落地后默认禁用
  （`enabled = false`）并保持未受信任状态，直至经显式审查后再启用。
- Full Access 和兼容 YOLO 的自动批准路径不会让带类型的 `ask` 把会话降级成弹窗提示。
  不可绕过的已注册拦截在 Full Access 下自动批准。带类型的 deny、灾难性的
  后台／无头安全拦截，以及仓库保护规则，在各自层适用的地方仍然失败关闭。
- 当 `approval_policy = "never"` 时，匹配的带类型 `ask` 被禁止，因为所需的提示
  无法显示。

运行时适配器运送审批决定的方式可能与交互式模态框不同。那条运送通道以及任何续接协议
与这个顺序契约是分开的；见[运行时 API](RUNTIME_API.md)。

## 项目覆盖层

`<workspace>/.codewhale/config.toml` 处的项目配置覆盖层不是权限规则层。
它只能把审批和沙箱姿态往更严格的方向移动：

- 审批：`auto` → `on-request`/`untrusted` → `never`；
- 沙箱：`danger-full-access` → `workspace-write` → `read-only`；
- shell 可用性：`true` 可以变成 `false`，反向不行。

项目配置不能添加凭据、钩子、提供商（provider）权限，也不能添加项目本地的 `permissions.toml`。
完整的覆盖层白名单见[配置](CONFIGURATION.md#按项目覆盖485)。

## 回归覆盖

该契约由各层中掌管对应决策的测试来演练：

- `authorization_order_contract_matches_documented_precedence` 通过公开的执行策略
  API 覆盖硬前缀拒绝，以及带类型的
  `layer → action → specificity → approval-mode` 序列。
- `hook_fold_deny_wins_over_ask_and_allow` 覆盖前台钩子的折叠。
- `non_bypassable_registered_tools_auto_approve_in_full_access` 覆盖已注册拦截。
- `full_access_permission_allow_cannot_bypass_background_catastrophic_floor`
  和 `full_access_permission_allow_cannot_bypass_repo_law` 覆盖后面的安全层
  如何覆盖一次记住的 allow。
- `project_merge_only_tightens_approval_and_sandbox_policy` 覆盖项目覆盖层的单调性。

聚焦命令：

```bash
cargo test -p codewhale-execpolicy --test authorization_order --locked
cargo test -p codewhale-tui --lib --locked full_access_permission_allow_cannot_bypass
cargo test -p codewhale-config --locked project_merge_only_tightens_approval_and_sandbox_policy
```

## 相关参考

- [配置](CONFIGURATION.md) — 规则模式、`/permissions`、钩子与项目覆盖层
- [模式](MODES.md) — Plan/Work/Operate 与权限姿态
- [沙箱威胁模型](SANDBOX.md) — 平台级强制执行与兜底
- [运行时 API](RUNTIME_API.md) — 审批事件与远程裁决
