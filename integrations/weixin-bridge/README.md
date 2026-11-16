# Weixin Bot Bridge

此 bridge 让微信个人账号通过扫码登录控制本地 `codewhale serve --http` runtime。
使用腾讯 iLink Bot 协议（参考 `@tencent-weixin/openclaw-weixin`）。

此 bridge 直接使用**个人微信账号**扫码登录授权，通过长轮询 `getUpdates` 收发消息。


## Quick Start

### 终端发起

#### 方式一、双终端启动

第一个终端启动 runtime：

```bash
export CODEWHALE_RUNTIME_TOKEN="$(openssl rand -hex 32)"
codewhale serve --http --host 127.0.0.1 --port 7878 --auth-token "$CODEWHALE_RUNTIME_TOKEN"
```

第二个终端启动 bridge：

```bash
cd integrations/weixin-bridge
export CODEWHALE_RUNTIME_TOKEN="<与上面相同的 token>"
npm start
```



#### 方式二、单终端启动

一条命令同时启动 runtime 和 bridge，自动生成并共用 token：

```bash
cd integrations/weixin-bridge
npm run bridge
```

按 `Ctrl-C` 同时停止两者。

### 微信端扫码接应

首次启动会打印文本二维码，用微信扫码登录：

![终端打印的登录二维码](../../docs/assets/weixin-bridge-qr-login.png)

二维码下方同时打印原始 URL，二维码显示异常时可手动打开。扫码窗口 5 分钟。

### 微信端验证信道效果

登录成功后，在微信里给这个 bot 发一条 `/status`。收到任何回复即表示链路已打通。

![微信端 /status 验证](../../docs/assets/weixin-bridge-status-verify.jpg)




## 安全模型

- `codewhale serve --http` 绑定于 `127.0.0.1`。
- `/v1/*` runtime 调用使用 `CODEWHALE_RUNTIME_TOKEN`。
- 默认只有扫码登录的那个微信（`ilink_user_id`）可以使用；其他聊天会收到一次带 `user_id` 的拒绝。
  设置 `WEIXIN_CHAT_ALLOWLIST` 后改用显式白名单。
- 仅支持私聊；暂不支持群聊。
- 工具审批：回复审批消息里的批准码，`1 k7m2fz` 允许、`2 k7m2fz` 拒绝。批准码一次有效、30 分钟过期，
  过期后 `/status` 会重新发出；批准本身在 Engine 中保持等待，不会因过期被拒绝。
  `/allow <approval_id>`、`/deny <approval_id>` 仍然可用。
- 每个状态目录带一个 `bridge.lock`；同一目录的第二个 bridge 进程会拒绝启动。
- bridge 主动向微信服务器发起长轮询请求，无需公网端口。

## 产品边界与验证

此目录是个人微信 iLink 与已有 Codewhale Engine 之间的消息适配器。
模型调用、工具执行、线程和审批仍由 Engine 管理。每个状态目录只能由一个
bridge 进程使用；白名单与本地 Runtime token 是操作员配置，不是多租户客户账户绑定。

托管助手应通过已登录的 Codewhale 账户配对微信，并恢复同一个 Agent、对话和
工作区。正常套餐内的计算可自动启动或恢复；额外费用和有后果的操作使用已有
授权与审批机制。账户、套餐、Computer 生命周期及计费由已有控制平面负责，
此 bridge 不实现这些托管服务。

本地模拟测试不代表真实扫码、腾讯消息收发、跨地区可用性、长期主动通知或
托管 Computer 已验证。腾讯公开协议描述的是当前客户端行为，并非完整服务器
合同；成功的发送接口响应也不等于收件人送达回执。


## 设置

登录凭证保存在 `WEIXIN_STATE_DIR`，再次启动无需重新扫码。线程映射与长轮询游标
写入同一目录，启动时会自动创建并探测可写性 —— 不可写则立即报错退出。

提示先持久化，再由后台任务交给 Engine；执行期间仍可收到 `/allow`、`/deny`
和 `/interrupt`。同一个 bot 账户重启后会恢复已接受的确切 turn，保存的回答与
待发送消息不会因事件连接断开而丢弃。旧状态没有账户绑定时，需要明确建立新
绑定；更换 bot 账户不会自动继承旧对话或发送旧回答。

Engine 必须同时声明 `turn_operation_idempotency` 和 `turn_operation_lookup`
能力，bridge 才会查询并恢复没有收到确认的提交。旧 Engine 的不确定提交会
保留供操作员核对，不会自动重新执行。发送结果不确定时，回答保留在私有的
线程映射文件中；`/status` 会提示，bridge 不会自动重发。操作员须先核对
Engine turn 与微信记录，避免重复执行或重复发送。同一账户仍有待核对工作时，
`/new`、`/resume` 和新提示不会覆盖它。更换 bot 账户后，明确发送 `/new`
会先把旧账户的完整状态保留在线程映射的私有 `retiredAccountStates` 中，
再建立新对话；旧提交与不确定回答仍须人工核对，不会自动执行或发送。
每个聊天最多保留 200 份旧账户记录，合计不超过 1 MiB；新增记录会超出任一
上限时，`/new` 会明确拒绝，且不会创建新线程或删除旧记录。已经超限的旧文件
也不会被自动裁剪。操作员应先停止 bridge、备份私有的 `thread-map.json`，
再核对并清理已确认不再需要的 `retiredAccountStates`；这些记录可能包含提示、
回答和旧上下文令牌，不应上传到公开问题或日志。

`/threads` 只显示当前账户与当前聊天拥有的最近对话，`/resume` 只接受该
账户和聊天已经绑定的对话。创建新对话会保留最多 200 条绑定记录；超出记录
范围的旧对话或其他聊天的对话不能通过任意线程 ID 接管。归属记录现在与聊天状态
分开保存在 `thread-map.json` 的 `threadOwners` 中。
升级时，只有由本 bridge 创建的线程才会保留给聊天：启动时 bridge 会向 runtime
确认一次当前绑定的线程，由桌面端、TUI 或其他客户端创建的线程会被移除，
该聊天需要发送 `/new`。

注意：bridge **不读取 `.env` 文件**，手动运行时环境变量必须通过 `export` 传入，
或使用 `node --env-file=.env src/index.mjs`。

systemd 部署时把变量写入 env 文件并由单元引用：

```bash
cd integrations/weixin-bridge
npm install --omit=dev
cp .env.example /etc/codewhale/weixin-bridge.env
sudoedit /etc/codewhale/weixin-bridge.env
node src/index.mjs
```

## 命令

- `/status`（`/状态`）：状态，并为等待中的审批重新发出批准码
- `/threads`（`/对话`）
- `/new`（`/新对话`）
- `/resume <thread_id>`
- `/model <name|default>`
- `/stop`、`/interrupt`（`/停止`）
- `/compact`
- `1 <批准码>` / `2 <批准码>`：允许或拒绝
- `/allow <approval_id> [remember]`、`/deny <approval_id>`
- `继续`（或 `more`）：查看剩余的回复

Engine 向你提问时，下一条消息就是回答：回复选项编号或文字；多个问题时每行回答一个。
其他所有内容均作为 Codewhale 提示发送。

## 微信的限制

以下来自腾讯官方客户端和公开 issue，不是完整的服务器合同：

- Bot 只能回复，且只能在你最后一条消息后的 24 小时内回复；bridge 按 23.5 小时计算。
- 每条你发出的消息之后，bridge 最多回复 8 条（`WEIXIN_MAX_SENDS_PER_MESSAGE`）。更长的回答
  先发 7 条加一条提示，回复「继续」查看剩余部分。窗口外完成的回答会等你下一条消息再发出，
  并提示你刚才的消息还没有运行。
- 回复之间至少间隔 1 秒；遇到 `rate limited` 时按 1 到 15 分钟退避后重试，不会连续重发。
- `getupdates` 或发送返回 `errcode -14`（会话过期）时，与腾讯客户端一样暂停所有 iLink
  调用 1 小时再重试；仍然过期时，删除 `account.json` 并重新扫码。
- `ret=0` 但没有 `message_id` 的发送记为 `unconfirmed`：腾讯可能在静默过滤这个账号的消息，
  `/status` 会提示。请到手机上确认。
- 每个微信用户只能绑定一个 ClawBot；在别处重新绑定会断开这里。不要反复扫码测试，
  有用户反馈因此被限制。
- Engine 生成回答时显示“正在输入”。

## 首次配对

扫码登录后，bridge 只接受扫码的那个微信。若需允许其他用户，在微信中发送 `/status`
查看各自的 `user_id`，写入 `WEIXIN_CHAT_ALLOWLIST` 并重启。`WEIXIN_ALLOW_UNLISTED=true`
会允许任何能联系到这个 bot 的人，仅用于排查。

## 环境变量

| 变量 | 必填 | 说明 |
|------|------|------|
| `CODEWHALE_RUNTIME_URL` | 否 | Runtime HTTP 地址（默认 `http://127.0.0.1:7878`） |
| `CODEWHALE_RUNTIME_TOKEN` | **是** | Runtime Bearer 令牌 |
| `CODEWHALE_WORKSPACE` | 否 | 工作区路径（默认 cwd） |
| `CODEWHALE_MODEL` | 否 | 模型名称（默认 `auto`） |
| `CODEWHALE_MODE` | 否 | 运行模式（默认 `agent`） |
| `WEIXIN_CHAT_ALLOWLIST` | 否 | 逗号分隔的允许用户 ID（默认只允许扫码的微信） |
| `WEIXIN_ALLOW_UNLISTED` | 否 | 允许任何用户（默认 `false`，仅用于排查） |
| `WEIXIN_STATE_DIR` | 否 | 状态持久化目录（默认 `/var/lib/codewhale-weixin-bot-bridge`） |
| `WEIXIN_THREAD_MAP_PATH` | 否 | 线程映射文件路径（默认 `<WEIXIN_STATE_DIR>/thread-map.json`） |
| `WEIXIN_MAX_REPLY_CHARS` | 否 | 单条回复最大字符数（默认 `3500`） |
| `WEIXIN_MAX_SENDS_PER_MESSAGE` | 否 | 每条用户消息后最多回复几条（默认 `8`，最小 `2`） |
| `CODEWHALE_TURN_TIMEOUT_MS` | 否 | 单次事件观察连接的时限（默认 `900000`）；断开后恢复观察，不中止 Engine turn |
| `WEIXIN_LONGPOLL_TIMEOUT_MS` | 否 | 长轮询超时（默认 `35000`） |

旧的 `WEXIN_*`（拼写错误）变量名仍作为已弃用别名被识别，启动时会打印一次弃用警告。

## 架构

```
微信客户端 ──getUpdates 长轮询──▶ Weixin Bot Bridge ──HTTP──▶ codewhale serve --http
                  ◀──sendMessage──                                  (127.0.0.1:7878)
```

Bridge 通过扫码获取 `bot_token`，然后长轮询 `POST /ilink/bot/getupdates`
以接收消息，并通过 `POST /ilink/bot/sendmessage` 发送回复。
所有消息均带有 `context_token` 以维持会话上下文。

## 微信与企业微信

本目录使用个人微信 iLink 扫码授权。`integrations/wecom-bridge` 使用企业微信
企业应用协议，是另一种账户和传输。微信公众号接口同样是独立路线；当前目录
不提供公众号回调、群聊或图片/文件上传下载。

协议参考：[腾讯官方客户端](https://github.com/Tencent/openclaw-weixin)及其
[协议说明](https://github.com/Tencent/openclaw-weixin/blob/main/docs/protocol.md)。
