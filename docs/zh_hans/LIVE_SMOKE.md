# 可选的实时冒烟运行

> 英文原文：[LIVE_SMOKE.md](../LIVE_SMOKE.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

本页是**手动、可选、绝不自动化**的。CI 里没有任何东西、没有测试、没有构建脚本、
也没有技能（skill）会运行这些命令。仓库的自动化测试套件在设计上就不含提供商（provider）；
至于在没有提供商时究竟断言了什么，见
[`crates/tui/assets/skills-catalog-matrix.json`](../../crates/tui/assets/skills-catalog-matrix.json)
以及目录矩阵（catalog-matrix）测试。

只有当你想要回答一个很窄的问题时才运行它：*在这台机器上，一条通往真实模型的真实路由
（route），是否返回了一份格式良好的回执（receipt）？*

## 一次实时冒烟运行能证明什么、不能证明什么

| 问题 | 这里能回答吗？ |
| --- | --- |
| 回执是否记录了我所请求的提供商/模型？ | 能。但这本身并不能证明是哪个网络端点处理了该请求。 |
| 这次运行是否产出可检视的路由/用量回执？ | 能，前提是 harness（冒烟测试驱动）走到了那个阶段。 |
| 一次响应能否证明我账号的权益状态？ | **不能。** 在得到旁证之前，提供商配置、认证/权益和 harness 行为都仍是候选原因。 |
| 模型是否在语义上挑对了技能？ | **不能。** 未度量。 |
| 技能注册表（registry）/目录/别名行为是否正确？ | **不能**——那是无提供商测试套件的职责。 |

把下面这些当作调查的起点，而不是已证实的故障类别：

- **提供商错误响应**——HTTP 401/403、未知模型、配额或区域错误，可能反映所配置的提供商/端点、
  凭据认证或权益、提供商可用性，也可能反映 harness 的路由/请求缺陷。仅凭响应本身无法区分它们。
- **回执或进程异常**——回执里的 `provider`/`model` 不对、回执字段缺失、崩溃，或未能使用隔离的
  状态目录，这些都是要调查 harness 的证据，但在归因之前仍然需要最小复现或其他旁证。

## 这些片段遵守的隔离规则

1. `env -i` 清空继承来的环境，所以你周围的 `HOME`、`CODEWHALE_HOME` 和 `*_API_KEY` 值不会被带进来。
   只有显式列在 `env` 行上的变量才会存活。
2. 只有 `CODEWHALE_HOME` 指向该任务专用的一次性目录，因此 Codewhale 配置、会话，以及随包安装的
   技能都落在临时状态里。`HOME` 被有意留空；冒烟运行绝不会改用它。
3. 凭据变量名由你自己指定（`CW_SMOKE_CRED_VAR`）。不会从提供商那里猜测任何东西。
4. 隔离出的子进程在关闭回显的情况下读取密钥，在 `EXIT`、`INT`、`HUP` 或 `TERM` 时恢复先前的终端
   状态，并且只在该子进程内导出它。该值不会落盘，也不会进入命令行参数或 shell 历史。
5. `PATH` 被显式转发，并且是唯一带过来的宿主机变量。

全程使用可移植的 `sh`；`stty` 和 `mktemp -d` 是仅有的非 POSIX 便利项，macOS 和主流 Linux 上都有。

## 第 1 步——创建一次性状态（两次运行都要）

```sh
CW_SMOKE_CODEWHALE_HOME="$(mktemp -d)" || exit 1
mkdir -p "$CW_SMOKE_CODEWHALE_HOME/tmp"
echo "scratch Codewhale state: $CW_SMOKE_CODEWHALE_HOME"
```

## 第 2 步——指定凭据变量

`CW_SMOKE_CRED_VAR` 必须是提供商所期望的那个变量名。Codewhale 为 Moonshot/Kimi 路由读取
`MOONSHOT_API_KEY`（或 `KIMI_API_KEY`），为 DeepSeek 路由读取 `DEEPSEEK_API_KEY`。

```sh
CW_SMOKE_CRED_VAR="MOONSHOT_API_KEY"     # you choose this; nothing is inferred
```

运行命令会在它隔离出的子进程里提示输入该值。它不会创建凭据文件。

## 第 3a 步——运行 A：Kimi K3

`kimi-k3` 是这个构建（build）认识的一个模型 id。所配置的提供商及其解析出的端点决定路由：
`--provider moonshot` 选中已配置的 Moonshot 路由；选择 `opencode_go` 则会选中那条另行配置的路由。
账号并不在两者之间做选择，harness 也不会根据响应在两者之间切换。请为你打算演练的路由设置
`CW_SMOKE_PROVIDER` / `CW_SMOKE_MODEL`。在提供商/端点配置、凭据访问和 harness 请求得到旁证之前，
模型未找到（model-not-found）响应都属于未归类的结果。

```sh
CW_SMOKE_PROVIDER="moonshot"
CW_SMOKE_MODEL="kimi-k3"
CW_SMOKE_EFFORT="medium"
CW_SMOKE_PROMPT="Reply with exactly: SMOKE OK"

env -i \
  PATH="$PATH" \
  TMPDIR="$CW_SMOKE_CODEWHALE_HOME/tmp" \
  CODEWHALE_HOME="$CW_SMOKE_CODEWHALE_HOME" \
  CW_SMOKE_CRED_VAR="$CW_SMOKE_CRED_VAR" \
  sh -c '
    CW_SMOKE_STTY_STATE="$(stty -g)" || exit 1
    restore_terminal() {
      stty "$CW_SMOKE_STTY_STATE" 2>/dev/null || :
    }
    trap "restore_terminal" EXIT
    trap "restore_terminal; exit 129" HUP
    trap "restore_terminal; exit 130" INT
    trap "restore_terminal; exit 143" TERM

    printf "Paste value for %s (input hidden): " "$CW_SMOKE_CRED_VAR" >&2
    stty -echo || exit 1
    if ! IFS= read -r CW_SMOKE_CRED; then
      printf "\nCredential input failed.\n" >&2
      exit 1
    fi
    restore_terminal
    trap - EXIT HUP INT TERM
    unset CW_SMOKE_STTY_STATE
    printf "\n" >&2

    export "$CW_SMOKE_CRED_VAR=$CW_SMOKE_CRED"
    unset CW_SMOKE_CRED
    exec codewhale exec \
      --provider "$1" --model "$2" --reasoning-effort "$3" --json "$4"
  ' sh "$CW_SMOKE_PROVIDER" "$CW_SMOKE_MODEL" "$CW_SMOKE_EFFORT" "$CW_SMOKE_PROMPT"
```

## 第 3b 步——运行 B：第二个提供商/模型（DeepSeek）

设置 `CW_SMOKE_CRED_VAR="DEEPSEEK_API_KEY"`，然后：

```sh
CW_SMOKE_PROVIDER="deepseek"
CW_SMOKE_MODEL="deepseek-v4-pro"
```

……再重新运行第 3a 步那段完全相同的 `env -i …` 代码块；它会提示输入一个新的凭据值。
对两个提供商运行*相同*形状的命令正是重点：结果不同是需要调查的观察结果，
而不是路由、权益或 harness 正确性的证明。提供商/端点配置、凭据、提供商健康状况，
以及生成的请求，全都仍是可能的解释。

## 第 4 步——可选：工具与推理回执

上面那个 `--json` 一次性调用记录的是 harness 声称的已解析路由；它并不能独立证明是哪个端点
处理了请求。如果还想看到工具目录和推理回执，请使用流式形式（仍然在同一个 `env -i` 包装里，
替换掉 `exec` 那一行）：

```sh
    exec codewhale exec --auto --max-turns 3 \
      --output-format stream-json \
      --provider "$1" --model "$2" --reasoning-effort "$3" "$4"
```

## 第 5 步——要记录什么

来自 `--json` 一次性调用的回执：

| 字段 | 期望 |
| --- | --- |
| `mode` | `one-shot` |
| `provider` | 与你传入的 `--provider` 完全一致 |
| `model` | 与你传入的 `--model` 完全一致 |
| `success` | `true` |
| `output` | 模型的文本；内容*不是*通过/失败判据 |

来自 `stream-json` 元数据回执：

| 字段 | 期望 |
| --- | --- |
| `provider`、`model` | 与你传入的标志一致 |
| `route_source` | 记录*为什么*选中了那条路由 |
| `reasoning_tokens` | 当回执报告推理时出现；缺失可能反映模型/提供商行为、配置，或 harness 的遗漏，需要旁证 |
| `tool_catalog_sha256` | 当提供了工具表面（tool surface）时出现 |
| `approval_posture` | 上面的 `--auto` 命令应为 `auto_tools` |
| `sandbox_posture` | 沙箱策略及其执行情况：`--auto` 默认是 Full Access；受限策略会标明 `local OS sandbox applied` 或 `policy only; no execution sandbox available`；外部执行后端会标明文件系统/网络隔离未经 Codewhale 验证 |
| `duration_ms`、`input_tokens`、`output_tokens` | 一次完成的运行中会出现 |

报告回执字段。**不要**粘贴凭据、密钥文件，或提供商的原始错误正文
（它们可能回显请求头）。

## 第 6 步——清理

```sh
rm -rf "$CW_SMOKE_CODEWHALE_HOME"
unset CW_SMOKE_CODEWHALE_HOME CW_SMOKE_CRED_VAR \
      CW_SMOKE_PROVIDER CW_SMOKE_MODEL CW_SMOKE_EFFORT CW_SMOKE_PROMPT
```

## 范围说明

一次绿色的实时冒烟运行是证据，说明所配置的实时尝试今天完成了。它本身并不能证明端点身份、
账号的持久权益，也不能证明不存在 harness 缺陷；那些主张需要另行旁证。它也没有说明技能选择、
别名解析、语言区域（locale）路由或提示词预算——这些都由 `crates/skills/src/catalog_matrix.rs`
以确定性、无提供商的方式覆盖。
