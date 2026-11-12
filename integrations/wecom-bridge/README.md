# WeCom (企业微信) Bridge

此 bridge 让**企业微信**用户通过智能机器人长连接控制本地 `codewhale serve --http` runtime。
使用企业微信智能机器人 API（长连接/WebSocket 模式），无需公网 IP。

与 `integrations/weixin-bridge`（个人微信 iLink Bot 协议）不同，此 bridge 面向企业微信组织内部使用，
通过 BotID + Secret 认证，支持企业通讯录权限管理。

## 安全模型

- `codewhale serve --http` 绑定于 `127.0.0.1`。
- `/v1/*` runtime 调用使用 `CODEWHALE_RUNTIME_TOKEN`。
- 企业微信用户必须加入白名单（`WECOM_CHAT_ALLOWLIST`），除非首次配对时设置 `WECOM_ALLOW_UNLISTED=true`。
- 支持私聊和群聊（群聊需要前缀 `/cw`）。
- 工具审批通过文本命令：`/allow <approval_id>` 或 `/deny <approval_id>`。
- 长连接模式无需公网端口。
- 企业微信只会看到 bridge 发送的提示、状态、线程摘要和审批消息；工作区、shell 和 runtime HTTP
  监听仍留在本机，并由 `CODEWHALE_RUNTIME_TOKEN` 保护。

## 前提

1. 拥有企业微信管理员权限
2. 在企业微信管理后台创建一个**智能机器人**（工作台 → 智能机器人 → 创建机器人）
3. 选择 **API 模式**，获取 BotID 和 Secret
4. （可选）配置机器人接收消息的格式

## 设置

```bash
cd /opt/codewhale/wecom-bridge
npm install --omit=dev
cp .env.example /etc/codewhale/wecom-bridge.env
sudoedit /etc/codewhale/wecom-bridge.env
node src/index.mjs
```

启动后 bridge 会自动建立 WebSocket 长连接，无需额外配置。

## 命令

| 命令 | 说明 |
|------|------|
| `/help` | 显示帮助 |
| `/status` | runtime 和工作区状态 |
| `/threads` | 此聊天创建的最近线程 |
| `/new` | 为此聊天创建新线程 |
| `/resume <thread_id>` | 重新绑定此聊天创建的线程；其他聊天或桌面端创建的线程会被拒绝 |
| `/model <name\|default>` | 设置或重置聊天模型 |
| `/interrupt` | 中断活动 turn |
| `/compact` | 压缩当前线程 |
| `/allow <approval_id>` | 批准待处理的工具调用 |
| `/deny <approval_id>` | 拒绝待处理的工具调用 |

其他所有内容均作为 Codewhale 提示发送。群聊中需要在消息前加 `/cw` 前缀。

## 线程归属

每个线程只属于创建它的聊天。bridge 在创建线程时把归属记录在
`thread-map.json` 中；`/threads` 只列出此聊天创建的线程，`/resume` 只接受
此聊天创建的线程 ID，其他 ID（包括桌面端、TUI 或其他聊天创建的线程）一律
得到同一条固定回复，且不会请求 runtime。

升级时，已有的 `thread-map.json` 中每个聊天只保留其当前绑定的线程，并且
bridge 会先向 runtime 确认一次：该线程必须由本 bridge 在当前配置的工作区中
创建。桌面端、TUI 或其他客户端创建的线程，以及被多个聊天同时引用的线程，
都会从聊天中移除，这些聊天需要发送 `/new` 创建新线程。启动时若无法连接
runtime，bridge 每 30 秒重试一次，期间返回同一条固定回复。已有的
`thread-map.json` 只记录各聊天当前绑定的线程，此前创建并已用 `/new` 离开的
线程不再被列出，也无法 `/resume`。升级后请检查该文件中的线程映射。

## 首次配对

1. 设置 `WECOM_ALLOW_UNLISTED=true` 启动 bridge。
2. 在企业微信中向机器人发送 `/status`。
3. Bridge 会拒绝并返回你的 `user_id`（或 `chat_id`）。
4. 将 `user_id` 加入 `WECOM_CHAT_ALLOWLIST`。
5. 将 `WECOM_ALLOW_UNLISTED` 改回 `false` 并重启 bridge。

## 环境变量

| 变量 | 必填 | 说明 |
|------|------|------|
| `CODEWHALE_RUNTIME_URL` | 否 | Runtime HTTP 地址（默认 `http://127.0.0.1:7878`） |
| `CODEWHALE_RUNTIME_TOKEN` | **是** | Runtime Bearer 令牌 |
| `CODEWHALE_WORKSPACE` | 否 | 工作区路径（默认 cwd） |
| `CODEWHALE_MODEL` | 否 | 模型名称（默认 `auto`） |
| `CODEWHALE_MODE` | 否 | 运行模式（默认 `agent`） |
| `WECOM_BOT_ID` | **是** | 企业微信智能机器人 BotID |
| `WECOM_BOT_SECRET` | **是** | 企业微信智能机器人 Secret |
| `WECOM_CHAT_ALLOWLIST` | 否 | 逗号分隔的允许用户 UserID |
| `WECOM_ALLOW_UNLISTED` | 否 | 首次配对模式（默认 `false`） |
| `WECOM_STATE_DIR` | 否 | 状态持久化目录 |
| `WECOM_THREAD_MAP_PATH` | 否 | 线程映射文件路径 |
| `WECOM_MAX_REPLY_CHARS` | 否 | 单条回复最大字符数（默认 `3500`） |
| `CODEWHALE_TURN_TIMEOUT_MS` | 否 | Turn 超时（默认 `900000`） |

## 架构

```
企业微信客户端 → 智能机器人长连接(WebSocket) → WeCom Bridge ──HTTP──→ codewhale serve --http
                    ◀── aibot_respond_msg ◀──                          (127.0.0.1:7878)
```

Bridge 使用 BotID + Secret 获取 access_token，建立 WebSocket 长连接。
接收 `aibot_msg_callback` 事件，通过 `aibot_respond_msg` 命令回复消息。
所有消息处理与 Codewhale Runtime API 交互，与 Feishu/Telegram bridge 共享相同逻辑。

## 与 weixin-bridge 的区别

| 特性 | weixin-bridge | wecom-bridge |
|------|---------------|--------------|
| 账号类型 | 个人微信 | 企业微信 |
| 登录方式 | 扫码登录 | BotID + Secret |
| 消息协议 | iLink Bot 长轮询 | 智能机器人 WebSocket |
| 认证方式 | 扫码获取 bot_token | API 获取 access_token |
| 组织管理 | 无 | 支持企业通讯录 |
| 公网需求 | 不需要 | 不需要 |
