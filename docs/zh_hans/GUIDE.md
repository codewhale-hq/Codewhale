# Codewhale 用户指南

> 英文原文：[GUIDE.md](../GUIDE.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

本指南面向你使用 Codewhale 的第一个小时。它涵盖了主要工作流程、重要安全控制，以及当你需要完整参考时接下来该看什么。

Codewhale 有更深入的参考文档，涵盖安装、配置、提供商（provider）、模式、快捷键、工具和运维。请将本页当作引导式走查，需要每个选项时再顺着"下一步"链接往下看。

## 1. 欢迎使用 Codewhale

Codewhale 是一个终端编码智能体（agent）。你从某个工作区运行它，交给它一个任务，它就能用结构化工具检查文件、运行命令、编辑代码，并带回证据汇报结果。

与普通聊天模型的重要区别在于，Codewhale 是围绕 “驾驭框架”（harness） 构建的：

- 它让活动工作区和会话保持可见。
- 它把每个回合都路由到明确的模式与审批规则。
- 它在转录中展示工具调用，而不是把工作藏起来。
- 它可以保存会话、分叉对话，并在之后继续。
- 它可以运行子智能体来执行专注的后台工作。

你可以用 Codewhale 回答小问题：

```text
解释此仓库中的身份验证流程。
```

也可以用它做多步工作：

```text
找到失败的验证路径，提出修复方案，等我批准了再编辑文件。
```

对于新仓库，请从保守的方式开始。在要求 Codewhale 修改文件之前，先让它探索和规划。这样会为你提供一条可审查的路径，也更容易及早发现错误的假设。

下一步：[ARCHITECTURE.md](ARCHITECTURE.md) 讲解内部 harness 与运行时模型。

## 2. 首次启动

在 macOS 或 Linux 上首次安装时，使用官方 GitHub Release。安装器会校验发布资源，
并在 `codewhale` 和 `codew` 两个命令名下提供同一运行时：

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Windows 用户请选择 [GitHub Releases](https://github.com/codewhale-hq/CodeWhale/releases/latest)
中的对应安装器或压缩包。已有的直接安装先运行 `codewhale update --check`，再运行
`codewhale update`。npm 和 Cargo 是次要打包方式；没有兼容预编译资源的平台仍可使用
受支持的 Cargo 源码构建路径。目录已占用、包管理器安装及 PATH 配置请参阅
[安装与迁移指南](INSTALL.md#recommended-official-github-releases)。Android/Termux 使用专用的
[预览压缩包或源码构建路径](INSTALL.md#android--termux-arm64)。

当你想要隔离的运行时，也可以用 Docker。当前公开发行的镜像版本是 0.10.0；
要测试当前 `main`，请[从源码构建](DOCKER.md#本地构建)。

```bash
docker volume create codewhale-home
docker run --rm -it \
  -e DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" \
  -v codewhale-home:/home/codewhale/.codewhale \
  -v "$PWD:/workspace" \
  -w /workspace \
  ghcr.io/hmbown/codewhale:latest
```

把安装目录加入 PATH 后，从你希望它工作的仓库或目录启动 Codewhale：

```bash
codewhale
```

使用 GitHub 安装器的默认目录时，在把该目录加入 PATH 之前，可以通过
`"$HOME/.local/bin/codewhale"` 启动。

首次启动时，Codewhale 只询问本次安装仍然需要的决定：无法推断语言时询问语言，未配置可用路由时询问提供商，文件夹需要决定时询问工作区信任。提供商步骤包含明确的离线路由。就绪界面随后打开真正的编辑器，保留命令行中提供的任务，或为当前文件夹建议第一个任务。

此后所有可选内容都保持可用。用 `/setup` 打开渐进式设置与修复指南，用 `/settings` 打开完整的带类型编辑器，想自定义内置工作约定时用 `/constitution`。本地化遥测选择只在工作区就绪后出现，不会阻塞编辑器。

DeepSeek 是默认提供商。如果你想在首次启动之前或之后配置它的 key，最直接的设置路径是：

```bash
codewhale auth set --provider deepseek
```

你也可以通过环境变量提供 key：

```bash
export DEEPSEEK_API_KEY="your-key"
codewhale
```

新的 Codewhale 配置存放在 `~/.codewhale/config.toml`。旧的 `~/.deepseek/config.toml` 文件仍受支持，供从旧名称迁移的用户使用。

用 `/constitution` 查看或更改常驻指引。设置完成后，运行一次 doctor 检查：

```bash
codewhale doctor
```

当你需要机器可读的报告用于提交 issue 时，用 JSON 形式：

```bash
codewhale doctor --json
```

两种形式默认都是离线的。
它们报告结构配置和字面上的未知/未探测凭证状态，不会加载工作区的 `.env` 凭据、打开 secret/OAuth 文件、探测钥匙串（keyring）、联系提供商或启动 MCP 服务器。只有有意需要该实时边界时，才使用 `--check-updates`、`--probe-api`、`--probe-local` 或 `--probe-mcp`。JSON 保持离线，不接受实时标志。

JSON 把凭据的 `source`（来源）与字面的 `availability`（可用性）分开报告。配置的环境、外部认证、OAuth、consent 和 secret-store 来源仍为 `not_probed`；它们的声明本身并不会让 Setup 或 fleet 就绪。只有结构上存在的字面配置值，或一条不需要凭据的路由，才能证明离线就绪。对于无法使用共享存储的路由上的旧版密钥存储哨兵（secret-store sentinel），会单独报告为 `secret_store_unavailable`/`unavailable`，而不是简单的"符合条件"或"未知"。

`doctor` 和 `doctor --json` 都还包含一项会话恢复诊断，它把旧会话文件名与当前存储对比，不读取会话内容，并报告以下之一： `isolated`、`no_legacy_sessions`、`migration_pending`、`migration_incomplete`、`migration_complete` 或 `scan_failed` 。
使用 `migration_pending` 或 `migration_incomplete` 作为提示，完成把会话从 `~/.deepseek` 迁移到 `~/.codewhale` 的工作——就是上面提到的旧路径迁移。显式设置 `CODEWHALE_HOME` 会抑制此环境检查。

下一步：[INSTALL.md](INSTALL.md) 涵盖各平台的安装路径，[CONFIGURATION.md](CONFIGURATION.md) 涵盖配置解析，[PROVIDERS.md](PROVIDERS.md) 涵盖提供商 ID 与凭据。

## 3. 你的第一个任务

从一个真实工作区里的只读任务开始：

```text
梳理仓库结构，并告诉我 CLI 入口点在哪里。
```

然后要一份有重点的计划：

```text
我想为空的配置值添加一个小型验证。
检查相关代码，并在编辑任何内容之前提出最小的安全更改。
```

当你准备好做编辑时，把验收标准说具体：

```text
落实你提出的验证。
将更改范围限制在配置解析内，添加或更新最窄的测试，并运行相关的检查。
```

好的首批提示词（prompt）包含四个要素：

- 你想要的结果。
- 你关心的文件、功能或行为。
- 哪些不在范围内。
- 什么算"验证通过"。

例如：

```text
修复配置加载器中错误的提供商报错信息。
不要更改提供商注册表。添加回归测试，并且只运行 config 包的测试。
```

如果你不确定 bug 在哪，直说：

```text
调查为什么 `codewhale doctor` 报告了错误的提供商。
暂时不要编辑文件。返回可能的原因、证据和提议的补丁计划。
```

面对不熟悉的代码，让调查和实现分步进行时 Codewhale 表现最好。对于很小且充分理解的改动，一个单独的实现请求就够了。

下一步：[MODES.md](MODES.md) 讲解何时使用 Plan、Work 和 Operate。

## 4. 了解界面

交互式 TUI 有几个稳定的区域：

- 头部（Header）：当前会话、活动模型、模式和总体状态。
- 转录区（对话记录，Transcript）：对话、工具调用、命令输出摘要和模型回复。
- 输入区（Composer）：你在这里输入提示、斜杠命令和文件提及。
- 任务面板（Tasks panel）：输入区下方的一条（或可选的侧栏），承载活动目标、待办列表和子智能体。行会保持整个会话——已完成的工作显示为"已完成"而不是消失——点击某一行（或对它按 `Enter`）会打开它的详情。
- 状态与底部区域：实时活动、排队的后续动作和简短命令提示。

当模型通过 `request_user_input` 提问时，问题面板会从对话记录底部展开，上方的对话仍然可见。面板打开时，可用 `PageUp` / `PageDown`、`Home` / `End`，或带 `Ctrl`、`Alt`、`Shift` 修饰键的 `↑` / `↓` 浏览对话记录。鼠标位于面板上方时，滚轮滚动对话记录；位于面板内时，滚动问题内容。滚轮浏览后，移动选项或开始输入会让当前内容重新进入视野。用 `↑` / `↓` 选择、`Enter` 确认、`←` / `h` 返回上一题，`Esc` 取消整组问题。每题都有可填写自定义回答的“其他”选项，输入文字时内容保持可见。相关按键见 [KEYBINDINGS.md](KEYBINDINGS.md)。

底部区域可配置。运行 `/statusline` 选择哪些内容可见，或在 `config.toml` 里设置 `[tui].status_items`。每个键只对应屏幕上的一样东西：`mode` 是姿态栏（posture bar）的 plan/act/operate 片区，而 `model`、`context_percent`、`cost`、`balance`（仅限预付费提供商：DeepSeek、DeepSeekCN、OpenRouter、SiliconFlow）、`cache`、`tokens`、`ttft`、`output_rate`、`workspace` 和 `git_branch` 是它下方指标行的片区。
省略 `status_items` 以保持内置默认；把它设为 `[]` 只保留帮助提示。

`workspace` 和 `git_branch` 默认关闭。工作区片区显示文件夹名称；链接工作树会包含父目录以区分同名文件夹。分支片区显示当前分支或游离 HEAD 的短 SHA，并用 `(wt)` 标记链接工作树。名称过长时，保留末尾并限制为 24 个显示列。Git 信息沿用每 15 秒的后台刷新机制，也可按需刷新；无法取得 Git 信息时省略分支片区。这些信息对应当前会话的工作区，完整路径仍可在 `/status` 查看。

`context_percent` 默认开启，并在任何占用率下都显示 `ctx NN%`——0.9.12 在 50% 以下保持沉默，使会话的大部分时间都没有上下文信号。该读数从 80% 起仍使用警示配色。

`status`、`agents`、`reasoning_replay`、`prefix_stability`、`last_tool_elapsed` 和 `rate_limit` 这些键在 0.9.13 中已退役：它们不驱动任何东西。旧的配置文件仍可加载——已退役的键会被忽略并在日志中给出警告。

`status_items` 负责组合这两行；另有两个尺寸预设决定每行绘制多少。`[tui].posture_bar` 和 `[tui].metrics_line` 各接受 `full`、`compact` 或 `hidden`。姿态栏默认使用 `full`，让活动控件始终可见；指标行默认使用 `compact`，保留选定的性能读数，同时去掉次要计数和帮助提示。它们也可以在运行时用 `/config posture_bar compact` 设置。TOML 中的值必须使用小写；`/config` 命令不区分大小写。`compact` 是该行走完最初几级逐级精简的过程后的样子：姿态栏保留权限与模式片区——以及属于建议而非装饰的容量警示——并舍弃时钟、计数和提示；指标行保留路由、上下文读数、成本和余额，并在空间足够时保留已选的 TTFT 和输出速率，舍弃次要计数与帮助提示。`hidden` 把该行交还给转录区。狭小的 tmux 面板可以隐藏两行而不动 `/statusline` 的组合。

`ttft` 和 `output_rate` 默认都开启，在 full 和 compact 行里都能用。`/statusline` 可以分别切换它们；Space 预览，Enter 保存，Esc 恢复你之前的设置。旧的 `session_metrics` 仍会同时启用这两项读数。这一对读数显示：`ttft 1.5s`（到首个流式 token 的平均时间）和 `120 avg tok/s`（本次会话中提供商报告的输出 token 总数，除以同一批调用的实测请求总秒数）。该速率包含连接建立、首 token 等待以及响应过程中的停顿，不包含工具执行和调用之间的空闲时间；它衡量的是请求的实际吞吐量，而非解码器速度。流式和非流式调用遵循相同规则；没有独立请求计时的回执，其 token 和时间都不计入。请求进行期间，保留上一次实测的平均值。两项读数与 `/status` 使用同一组累加器，`/status` 会打印完整数据。缺少证据时省略，而不是估算。在窄行上，这一对会先于成本和上下文读数被舍弃。

每一次文件读取、命令和编辑，都会在发生时出现在转录区。`/receipts` 会逐条列出本次会话做过的每个动作，详见 [Codewhale 记录了什么](#codewhale-记录了什么)。如果某条命令失败，把可见的失败输出作为你下一条指令的一部分，而不是从头再来。

输入区接受普通提示和斜杠命令。输入 `/` 可以发现可用命令。想让模型专注于某个特定文件或目录而不是广泛搜索时，使用文件提及。

当一个回合跨越多个步骤时，工作栏很有用。它让目标、待办列表和智能体状态保持可见，同时转录区继续增长——包括在工作落定之后，这样你仍然可以打开看看发生了什么。

键盘快捷键因上下文、终端和平台而异。本指南不重复完整的快捷键目录，以免与 TUI 脱节。

下一步：[KEYBINDINGS.md](KEYBINDINGS.md) 是完整的快捷键参考。

## 5. 模式

Codewhale 有三种可见的 TUI 模式：

| 模式 | 用于 | 默认权限级别 |
| --- | --- | --- |
| Plan | 改动前的探索、设计与审查 | 只读调查 |
| Work | 常规的多步编码工作 | 带审批门禁的工具使用 |
| Operate | 直接工作，外加并行或后台协调 | 工具遵循当前权限级别；需要时委派 |

从 TUI 里用模式选择器切换模式：

```text
/mode
```

或直接切换：

```text
/mode plan
/mode work
/mode operate
```

Plan 模式是在陌生仓库里开始的最安全位置。它用于检查和决策，不做文件编辑。当 Plan 回合成功完成非空回复时，Codewhale 会询问如何继续，即使计划只是普通文本。
**Work（询问）** 和 **Work（自动审核）** 会以对应权限切换到 Work，并将获批回复的原文作为下一条消息发送。该回合已有的待办事项会保留；纯文本计划会添加一个待处理事项，不会替换其他工作。如果 Codewhale 无法准备工作状态，它会留在 Plan，不会开始执行任务。
**继续规划** 或 `Esc` 留在 Plan。你也可以在那里输入反馈来修订计划。只有在输入框为空且没有打开其他视图时才会出现这个提问；`/mode work` 始终可用。

Work 模式是大多数贡献工作的默认模式。它允许 Codewhale 读文件、跑检查、编辑文件，同时把有风险的动作留在审批门禁之后。

Operate 保持直接的工具面及其审批、沙箱、shell、ask 规则和仓库保护。小型或紧密耦合的工作直接处理。多步骤委派使用简洁的 Workflow 计划，明确依赖关系、工作范围，并在步骤间传递完成证据。Fleet 配置和管理的就是这些子智能体及其角色。一个范围明确、可独立完成的任务可以直接交给一个智能体；后续工作通过 `followup` 继续使用同一个智能体。繁重的工作也可以用 `codewhale dispatch "<task>"` 交给云端计算机运行，它通过已登录的账户进行，并附带报价和欧盟计算资源确认。

对于你信任的工作区，如果你确实希望动作不经审批提示就继续，可以用 `Shift+Tab` 选择 Full Access（完全访问）权限级别。不要在你不信任的仓库里使用 Full Access。

模式与模型路由是分开的。输入区空闲时 `Tab` 循环切换可见模式，而 `/model auto` 控制回合的模型与思考选择。

你也可以在 `/config` 里通过编辑审批模式来改变审批行为。只有当你理解它会如何改变工具执行时才使用它。

下一步：[MODES.md](MODES.md) 有完整的模式、审批和信任模式参考。

## 6. 斜杠命令

斜杠命令在输入区里输入。当你想要直接改变 Codewhale 状态，而不是用自然语言让模型去做时，它们很有用。

对首次用户常用的命令：

| 命令 | 用途 |
| --- | --- |
| `/mode` | 打开模式选择器，或用 `/mode agent` 切换 |
| `/model` | 选择模型，或用 `/model auto` |
| `/provider` | 选择活动的 API 提供商|
| `/fleet` | 打开当前所选 fleet 的成员花名册 |
| `/fleet saved` | 选择或切换已命名保存的 fleet |
| `/goal` | 设置一个智能体跨回合持续追求的持久目标；裸 `/goal` 显示进度 |
| `/workflow` | 把当前工作编排为 Workflow；`status`、`cancel`、`settings` 无需模型回合即可回答 |
| `/workflows` | 打开实时 Workflow 运行仪表盘：该工作区日志记录的每一次运行，含阶段、子项、进度和主机侧取消 |
| `/config` | 编辑运行时与提供商设置 |
| `/statusline` | 选择哪些底部状态芯片可见 |
| `/receipts` | 列出本次会话做过的事：改动的文件、运行的命令、网页和 MCP 调用、智能体、审批及审批人、失败 |
| `/compact` | 压缩长上下文以回收 token 预算 |
| `/copy` | 把最近一条已完成的助手回复复制到剪贴板 |
| `/review` | 请求结构化的审查工作流 |
| `/memory` | 启用时检查或管理记忆 |
| `/mcp` | 配置或检查 MCP 服务器集成 |
| `/plugin` | 审查和管理默认禁用的本地插件包 |
| `/rc` | 把此确切会话交给已登录的 Codewhale 网页应用 |

工具箱命令直接输入即可搜索：`/models` 拉取实时端点 ID，`/modeldb` 打开内置模型参考，`/rlm` 把文件或一段文本加载进工作上下文，在会话剩余时间里保持可用。

想切离默认的 DeepSeek 路由时用 `/provider`。Provider ID、环境变量、模型默认值和能力说明都保留在提供商注册表文档里。

软自动多智能体工作：[AUTOMATIC_WORKFLOWS.md](AUTOMATIC_WORKFLOWS.md)。

以 bot 身份发布 Codewhale 的 PR 审查：[GITHUB_APP.md](../GITHUB_APP.md)。

面向持久多智能体工作的下一步：[FLEET_WORKFLOW_TUTORIAL.md](./FLEET_WORKFLOW_TUTORIAL.md) 带你走一遍 fleet 任务规范、监控和 Workflow 编写。

Fleet 是持久花名册对外的名称。`codewhale fleet …` 是命令，`/fleet` 是斜杠命令。Fleet 这个名字也用在那些必须跨版本保持稳定的地方：持久账本 `.codewhale/fleet.jsonl`、已保存的花名册 `fleets/<name>.toml`、`[fleet]` 配置表，以及 `codewhale workflow run --fleet` 标志。

想让 Codewhale 每回合自己选模型和思考级别时，用 `/model auto`。当 DeepSeek 路由模型可用时，Auto 可以在脱敏清单中选取任何可运行的 provider/模型组合。该分类会把最新请求（上限 4,000 字符）加上最多六条最近上下文行的有界摘要（每条 900 字符）发送到 `DeepSeek / deepseek-v4-flash`。凭据、端点和提供商错误文本不会包含在清单里。没有该路由器时，Auto 使用本地的、感知提供商的启发式方法，不发送任何路由请求。如果分类尝试未通过验证或出错，Auto 回退到该启发式方法，同时把尝试过的分类器数据路径保留在回合回执中。

`/model` 选择器会说明哪条数据路径可用，并显示最后解析的路由。`Ctrl+O` 打开所选或当前回合的推理详情；`Ctrl+Alt+O`（或 `/turn inspect`）打开整回合的回合检查器（Turn Inspector），其模型路由区记录具体的 provider/模型、strong/fast 配对、所选层级、选择范围、路由原因，以及分类器是否收到了路由上下文。当你需要可重复的比较、严格的提供商边界或完全不要分类请求时，使用固定模型。

会话变长、模型开始承载太多历史记录时，用 `/compact`。压缩会用简洁的工作摘要换取原始转录细节。

本指南有意不列出每条命令。命令面比上手流程变化更频繁，你在会话里时，TUI 命令面板才是事实来源。

下一步：[CONFIGURATION.md](CONFIGURATION.md) 涵盖运行时设置，[MCP.md](MCP.md) 涵盖模型上下文协议（MCP，Model Context Protocol）集成。[PLUGIN_BUNDLES.md](PLUGIN_BUNDLES.md) 涵盖默认禁用的包清单、能力审查和带命名空间的 Skill/MCP 激活边界。

## 7. 使用工具

Codewhale 的工具是结构化操作。模型不只是产出文字，还能调用工具来检查和改变工作区。

工具支撑的工作示例包括：

- 解释文件之前先读它。
- 提出重构之前先搜索调用点。
- 运行一条有重点的测试命令。
- 应用一个小补丁。
- 为并行调查打开一个子智能体。

工具使用由模式、审批和沙箱策略约束。确切行为取决于当前模式和配置，但基本规则很简单：只读探索用 Plan 开始，常规改动用 Work，Full Access 留给受信任的自动化。

工作区边界很重要。Codewhale 应该在你启动它的目录或你配置的工作区里工作。当任务应该留在仓库内时要说清楚：

```text
只检查和编辑此仓库下的文件，不要改动上级目录或全局配置。
```

当命令需要网络、在工作区外写入或有风险的 shell 操作时，除非你配置了更宽松的行为，否则期待一个审批提示。

好的工具指令是具体的：

```text
运行覆盖此解析器更改的最窄测试。
如果失败，报告失败并在扩大测试范围之前停止。
```

避免在专注修复期间要求广泛的清理。较小的工具范围使转录更易于审查，最终的差异更易于合并。

下一步：[TOOL_SURFACE.md](TOOL_SURFACE.md) 列出工具面，[SANDBOX.md](SANDBOX.md) 讲解沙箱行为。

### Codewhale 记录了什么

Codewhale 把这些记录保存在你的机器上，位于 `~/.codewhale/` 下：

- **会话。** `sessions/<id>.json` 保存完整的对话，包括每一次工具调用及其结果文本。应用和 `codewhale serve` 的线程，则把每次调用作为一个回合条目（turn item）保存在 `tasks/runtime/` 下，含其输入、状态、开始和结束时间以及结构化结果。
- **审批。** `sessions/<id>/approval_receipts.jsonl` 记录 Codewhale 请求过的每一次审批、决定，以及决定人：你、某条会话规则，或当前生效的权限级别。应用线程也会在其事件日志里记录每个决定。没有询问就运行的调用（Full Access、允许规则、已记住的授权）没有审批记录；每个回合所处的权限级别会随该回合一起保存。
- **撤销点。** 工作区快照让你可以回退。`/undo` 一步撤销你的上一次请求：它改动的每个文件，以及这次请求和回复。它在任何权限级别下都可用，并列出恢复了哪些文件。如果其中某个文件在请求之后又被改动，`/undo` 不做任何更改，并指出是哪个文件。如果某次请求在 Codewhale 记录其改动终点之前被中断，`/undo` 会说明这一点；`/undo force` 仍会撤销它，包括此后对这些文件所做的编辑。`/restore` 把整个文件夹回滚到你选择的时间点，会覆盖之后的编辑，因此需要 `/trust on` 或 Full Access。Esc Esc 只回退对话；之后再运行 `/undo` 可以把文件恢复。
- **安全事件。** `audit.log` 记录凭据变更、hook 环境变量的键名、压缩（compaction）过程、终端的审批路由，以及 Auto-Review 的裁决。它不是一份会话做过什么的清单。

要查看一次会话做了什么，运行 `/receipts`，或在 shell 里运行：

```bash
codewhale receipts --last
codewhale receipts <session-id> --format json
```

回执会说明它自己看不到什么。终端会话的回执，根据每个回合的工作区快照列出命令改动过的文件；Runtime 线程的回执只列出文件工具。终端会话不会保存一次成功命令的退出码，也不会保存每次调用花了多长时间。Codewhale 在开始之前就拦下的调用（Auto-Review、某条策略、无效输入）会被列为已拦截并附上原因，不计入已运行。完整契约见 [RECEIPTS.md](../RECEIPTS.md)。

## 8. 子智能体与并行工作

子智能体是后台子智能体（child agent）。父会话给子智能体一个专注的任务，收到一个 agent id，然后可以在子智能体运行时继续工作。

主要的编排工具是：

- `agent`：带任务和角色启动一个专注的子智能体。子智能体在后台运行，返回一份紧凑回执加转录句柄。

你通常不需要直接调用这些工具。用自然语言请求并行工作：

```text
为 config 包打开一个只读探索器，为 TUI 提供商选择器打开另一个。让两者在规划修复之前返回文件引用和风险。
```

有用的角色包括：

| 角色 | 适合 |
| --- | --- |
| `general` | 多步任务；未指定角色时的默认值 |
| `explore` | 只读代码梳理 |
| `plan` | 设计与迁移规划 |
| `review` | 对已有改动的 bug 聚焦审查 |
| `implementer` | 规格明确的编辑 |
| `verifier` | 运行检查并报告通过/失败证据 |

子智能体在可以干净切分工作的时候最有用。不要为微小编辑使用它们，也不要让多个智能体同时写入相同文件。

### 长时间工作如何保持连贯

跨越多个回合的工作不依赖无限增长的聊天转录。这是普通 Agent 行为——不需要打开任何东西，也没有单独的工作流要学：

- 工作上下文在整个会话中保持加载。大段源材料和持久转录作为数据保存，智能体可以搜索和切片，有用的变量与导入跨回合存活。
- Workflow 组合独立的 `task(...)` 调用和并行扇出。
- `agent` 消息与后续动作直接协调活动的子智能体。
- 目标（Goals）在工作期间保留持久目标。

`/rlm <file-or-text>` 把工作上下文指向一个特定文件或一段文本。历史上一度存在的动作形态 `rlm` 工具仍然注册着，只为了让旧会话能回放，并且刻意不教给新的模型回合。

Codewhale 还可以在 `.codewhale/harness/state.json` 维护一个小型项目级账本：有证据支撑的提示备注、可复用的子智能体简报和 skill 路由提示。之后的回合会把它当作不受信任的补充指导接收，绝不是权威或可执行指令。读取它是自动的；添加或删除条目要走正常的审批回执。它和个人记忆是分开的，绝不能保存密钥、草稿转录或未经证实的说法。

下一步：[SUBAGENTS.md](SUBAGENTS.md) 涵盖角色、生命周期、并发和输出契约。

## 9. 技能（Skills）

技能是可复用的指令包。一个技能通常是 `SKILL.md` 文件，教 Codewhale 如何执行某个重复工作流、使用某类工具，或遵循某项项目约定。

当任务有可重复的流程时使用技能：

- 审查某一类 PR。
- 处理某种文档或电子表格格式。
- 遵循团队发布检查清单。
- 使用项目特定的记忆或 wiki 工作流。

在 TUI 里，`/skill <name>` 在可用时激活技能，裸 `/skills` 打开技能管理器（仅限自有清单，无网络）。用 `/skills <prefix>`、`/skills inspect`、`/skills --remote`、`/skills suggest <task>` 或 `/skills sync` 走文本/注册表路径。建议会对远程目录排序，但绝不安装或激活任何东西。命令面板也能把技能条目和普通斜杠命令一起展示。

好的技能要窄。它们应该告诉模型遵循什么工作流、收集什么证据、避免什么。它们不应该隐藏凭据或取代正常的仓库文档。

如果仓库有自己的指令，请把它们当作活动工作的一部分。编辑前先读本地指南，并让你的贡献保持在仓库约定之内。

下一步：见 [SKILLS.md](SKILLS.md) 了解管理器、所有权和来源规则；[CLAUDE_PLUGIN_COMPAT.md](../CLAUDE_PLUGIN_COMPAT.md) 了解 Claude Code 技能/插件兼容性；[CONFIGURATION.md](CONFIGURATION.md) 了解配置路径与项目权威。

## 10. 获取帮助

从 doctor 输出开始：

```bash
codewhale doctor
```

提交详细 issue 时用 JSON：

```bash
codewhale doctor --json
```

对于认证问题，用结构化的来源状态确认声明了什么。Doctor 刻意不检查环境、secret-store、钥匙串或 OAuth token 的值。当实时检查合适时，用 `codewhale doctor --probe-api` 选择加入（本地端点用 `--probe-local`）。

对于提供商问题，确认活动的提供商和模型：

```text
/provider
/model
```

会话又长又乱时，用 `/compact` 减轻上下文压力，或在同一工作区开一个新会话并总结你需要的东西。

报告 issue 时，请包含：

- Codewhale 版本。
- 安装方式。
- 操作系统和终端。
- 提供商和模型。
- 确切的命令或提示。
- 相关的 doctor 输出。
- 问题是否在新工作区里也出现。

不要把 API key、私有源码或密钥粘贴进公开 issue。

下一步：[OPERATIONS_RUNBOOK.md](OPERATIONS_RUNBOOK.md) 有运维分诊与恢复步骤。

## 常见问题（FAQ）

### Codewhale 只支持 DeepSeek 吗？

DeepSeek 是默认且一等的路由，但 Codewhale 也支持其他托管和本地的 OpenAI 兼容提供商。用 `/provider` 或 `codewhale --provider <id>` 选择提供商。配置非默认路由时，请打开提供商注册表参考。

### 我应该先用哪个模式？

陌生代码用 Plan，常规实现用 Work，只有在你信任、可以接受自动执行的仓库里才用 Full Access。

### 为什么 Codewhale 运行命令前要问我？

审批是安全模型的一部分。Shell 命令、付费工具、写入以及预期工作区之外的动作都可能产生副作用。审批提示让你在让模型做有用工作的同时保持控制。

### 我如何在 macOS 上运行一个 Python 文件？

在包含该文件的文件夹里打开终端并运行：

```bash
python3 your_file.py
```

如果 macOS 提示 `python3` 缺失，从 [python.org](https://www.python.org/downloads/macos/) 或 Homebrew 安装 Python：

```bash
brew install python
```

在 Codewhale 里，让智能体检查文件并用 `python3 your_file.py` 运行它。如果脚本需要包，先在虚拟环境里安装：

```bash
python3 -m venv .venv
source .venv/bin/activate
python3 -m pip install -r requirements.txt
python3 your_file.py
```

### 我的配置存放在哪里？

新的 Codewhale 配置使用 `~/.codewhale/config.toml`。旧的 `~/.deepseek/config.toml` 为兼容性仍然受支持。当工作区配置存在时，项目覆盖也可能影响行为。

### 如何让成本可预测？

用 `/model auto` 做路由，需要严格配置时选择固定模型，并压缩长会话。对更大的任务，让 Codewhale 先规划再实现，这样你就不会把 token 花在错误的路线上。

### 如何继续之前的工作？

Codewhale 会保存会话。用 README 和模式指南里讲到的会话选择器或 resume/continue CLI 路径。对于有风险的实验，在改变方向前先分叉（fork）会话。

`/sessions` 选择器以当前工作区为范围启动，这样恢复会保持挂在打开的项目上。在选择器里按 `a` 显示所有工作区的会话，或在恢复某个特定 id 之前运行 `codewhale sessions` 列出所有已保存会话及其最后更新时间。

要归档持久记录及其工件（artifact），运行：

```sh
codewhale sessions export <id-or-unique-prefix> --output session.tar.xz
```

归档包含 `session.json`、可移植的 `container.json`、一份清单（manifest），以及 `artifacts/` 下的普通文件。`--skip-artifacts` 只导出记录本身，`--compression 0` 到 `9` 选择 xz 预设（默认 `6`），`--force` 替换已存在的输出。请把归档存放在会话存储之外。符号链接会被跳过；带链接的工件根目录、硬链接、不可移植的文件名，以及超过 64 层目录或 100,000 个条目的目录树，都会导致导出失败，且不会替换目标文件。

与经过脱敏的 Markdown `/export` 不同，这些归档保留未脱敏的会话内容，包括系统提示词、思考内容、工具调用及其结果、日志分支和审批回执。请解压出 `session.json`，并在 TUI 里用 `/load` 打开；`/resume` 只导入对话。解压出的工件仍是独立文件，`/load` 不会把它们装入工件存储。如果必须让所有工件反映同一时刻的状态，请在归档前暂停写入；增长中的文件会被限制在其记录的大小内，缩小的文件会使导出中止。

要从网页应用继续当前正在运行的会话，输入 `/rc` 或用 `codewhale rc` 启动。在系统浏览器里批准一次性验证码。租约有效期间，浏览器拥有新的提示和审批，终端只作为可查看的只读界面。连接后，横幅和一条转录备注会显示实时会话链接（`https://app.codewhale.net/session?run=…`）；`/rc open` 在浏览器里打开它，`/rc link` 打印它。`/rc status` 显示归属，`/rc stop` 把它交回终端，interrupt 仍然可用。断开的连接会保持本地输入锁定，直到最后一个网页端租约过期，这样两个控制端永远不会互相抢占。从一个终端登记的每个文件夹共享同一个稳定的设备 id，因此网页应用每台机器列出一台电脑，而不是每个会话一台。

> 注（2026-09-14）：根据 2026-09-14 的产品客户端决定，app.codewhale.net 的托管网页应用将分阶段下线；原生 GPUI 桌面应用（私有 codehwhale-gpui 仓库，阶段规划见 docs/TRANSITION.md）是承接界面。网页应用存续期间 `/rc` 继续可用。

### 模型糊涂了，我该怎么办？

停下来，重新陈述目标、约束和当前证据。如果转录很长，用 `/compact`，或带简短交接开一个新会话。如果是运维问题，运行 `codewhale doctor` 并检查报告的配置与提供商状态。

### 项目规则应该放在提示里还是文件里？

持久性的项目规则用仓库文件，回合特定的意图用提示。如果某个工作流跨项目重复出现，考虑把它做成技能。

### Codewhale 能编辑当前仓库之外的文件吗？

这取决于工作区边界、沙箱设置、信任模式和审批策略。做贡献工作时，让指令保持在当前仓库范围内，除非你确实需要别的。

### 学完本指南后我该去哪？

读与你正在改动的东西相关的重点参考。对大多数用户，接下来的页面是安装、配置、提供商、模式、快捷键、工具和子智能体。

下一步：[INSTALL.md](INSTALL.md)、[CONFIGURATION.md](CONFIGURATION.md)、[PROVIDERS.md](PROVIDERS.md)、[MODES.md](MODES.md) 和 [TOOL_SURFACE.md](TOOL_SURFACE.md)。
