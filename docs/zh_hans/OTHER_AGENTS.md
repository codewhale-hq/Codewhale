# 在 Codewhale 中运行其他智能体

> [English](../OTHER_AGENTS.md)

外部智能体会话由安装在 Codewhale 所在电脑上的智能体 CLI 执行。Codewhale 保存会话、对话和审批记录；
外部智能体使用自己的账户、模型和工具执行每一轮。两者通过
[Agent Client Protocol (ACP)](https://agentclientprotocol.com/) 连接。
外部智能体不是 Codewhale 的模型服务商，也不是子智能体。如果你想让编辑器连接 **到** Codewhale，
请参阅 [Codewhale 的 ACP 适配器](../RUNTIME_API.md#acp-stdio-adapter-codewhale-serve---acp)。

## 安装与登录

1. 在运行 Codewhale 的电脑上安装智能体，而不是只装在用来连接的设备上。
   [智能体注册表](../../crates/engine/src/fleet/agents.rs) 列出了可识别的智能体、最低版本和安装链接。
2. 在智能体自己的 CLI 中登录或配置模型，例如 OpenCode 的 `opencode auth login`。
   Codewhale 不会向外部智能体传递保存在 Codewhale 中的服务商密钥。
3. 确认服务端的 `PATH` 能找到智能体。使用 Claude Code、Codex 或 Muse Code 时，还需安装对应的 ACP
   适配器，并让服务端的 `PATH` 能找到它。Codewhale 不会替你下载智能体或适配器。

| 智能体 | 安装适配器 |
| --- | --- |
| Claude Code | `npm i -g @agentclientprotocol/claude-agent-acp` |
| Codex | `npm i -g @agentclientprotocol/codex-acp` |
| Muse Code | `npm i -g @bex-co/muse-code-acp` |

## 启动会话

你可以直接通过 Runtime API 创建会话。在项目目录下打开 Bash，启动本地服务。请妥善保管令牌。

```bash
export CODEWHALE_RUNTIME_TOKEN="$(openssl rand -hex 32)"
codewhale app-server --http --host 127.0.0.1 --port 7878 &
server_pid=$!

base=http://127.0.0.1:7878
api() { curl -sS --fail-with-body -H "Authorization: Bearer $CODEWHALE_RUNTIME_TOKEN" "$@"; }
# 等服务启动后再执行。
api "$base/v1/agents/external"
```

从返回结果中选择标有 `"runnable": true` 的智能体 `id`。这表示 Codewhale 能找到并驱动它，
不代表它已登录。下面以 OpenCode 为例，用 `jq` 构造 JSON，项目路径有空格也能正常传入。

```bash
agent=opencode
thread=$(api "$base/v1/threads" -H 'Content-Type: application/json' \
  -d "$(jq -n --arg workspace "$PWD" --arg agent "$agent" \
    '{workspace:$workspace, permission_posture:"ask", executor:{kind:"acp", agent_id:$agent}}')")
thread_id=$(jq -er .id <<< "$thread")
api "$base/v1/threads/$thread_id/turns" -H 'Content-Type: application/json' \
  -d '{"prompt":"请概述这个项目，不要修改文件。"}'
api -N "$base/v1/threads/$thread_id/events"
```

事件流会持续输出更新。按 Ctrl+C 只是停止查看，不会取消这一轮。
用 `GET /v1/threads/{id}` 查看这一轮和待审批请求；用
`POST /v1/approvals/{approval_id}` 发送 `{"decision":"allow"}` 或
`{"decision":"deny"}`。要取消当前轮次，调用 `POST /v1/threads/{id}/turns/{turn_id}/interrupt`。
用完后运行 `kill "$server_pid"` 停止服务。要恢复会话，用原来的 Codewhale 和智能体状态重启服务，
再用保存的会话 ID 调用 `POST /v1/threads/{id}/resume`。

不指定模型时，使用智能体自己的默认模型。如果智能体支持选择模型，可调用
`GET /v1/agents/external/{id}/options?cwd=...`，其中 `cwd` 是经过 URL 编码的项目绝对路径。
创建会话时，将智能体提供的选项值填入 `executor.config.model`。这里不使用 Codewhale 的模型 ID。

## Codewhale 能控制什么

- **审批：** Ask 模式会显示智能体通过 ACP 发来的审批请求。如果智能体有自己的放行规则，
  它可能不发请求就执行。Codewhale 不会对外部工具施加沙箱。请在智能体中配置权限，只运行你信任的智能体。
- **恢复：** 智能体支持 ACP resume 或 load 时，Codewhale 会恢复原来那条会话；
  否则会报告无法恢复，不会另开一条代替。
- **限制：** 外部会话不支持通过 Codewhale 追加指令（steer）、压缩上下文、分叉、输入图片，
  或为单轮更换模型。用量只在智能体主动报告时显示，Codewhale 不知道实际费用。

## 无法启动时

| 错误码 | 处理方法 |
| --- | --- |
| `not_installed` | 安装 CLI，检查服务端的 `PATH`。 |
| `adapter_missing` | 安装所需的 ACP 适配器，检查服务端的 `PATH`。 |
| `below_min_version` | 把 CLI 更新到提示的最低版本或更新版本。 |
| `no_acp` | 检查 CLI 或适配器的版本，以及能否以 ACP 模式启动。 |
| `needs_sign_in` | 在智能体自己的 CLI 中登录，然后重试。 |

请求字段、权限策略和事件格式请参阅
[Runtime API 契约](../RUNTIME_API.md#sessions-on-other-agents-external-agent-threads)。
