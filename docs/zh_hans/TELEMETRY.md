# Codewhale 产品遥测

> 英文原文：[TELEMETRY.md](../TELEMETRY.md)（schema v3 / 告知版本 5）。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

**当前源码默认开启使用统计，可随时退出。** 首次交互启动显示本地化、非阻塞告知，列明 **Codewhale 和 PostHog**，并提供关闭入口。第 `5` 版告知说明这一默认开启政策；显示告知不等于代替用户记录同意。新安装缺少旧同意记录时仍默认开启；此前明确退出的选择继续有效，隐私状态不可读时保持关闭。环境和命令行关闭开关仍然优先。

PostHog 转发是可选的，在采集服务的运维方明确配置项目令牌和获准的区域主机、并记录预发布验证证据（证明实际出口路径不会转发原始客户端 IP）之前不会生效。源码就绪不代表已经部署、已激活处理服务、已有出口回执或已设置保留期限。

可用 `codewhale config telemetry` 阅读告知。通过 `/settings` 或 `codewhale config set telemetry false` 关闭统计。在 Settings 中明确重新开启，或执行 `codewhale config set telemetry true`，会更新现有偏好和隐私记录，供新会话使用。兼容命令 `codewhale config telemetry --accept-notice 5` 仍可使用，但新安装无需执行该命令。采集内容从不包含工作内容或凭据。

Codewhale 不会收集对话、代码、提示词、文件、文件/仓库/分支名称、模型内容或凭据。它不发送任何按回合或按工具的时间线。它只发送下面这个封闭的聚合 schema：版本和平台类别、会话时长/结果、功能/错误计数器，以及一个每 90 天轮换一次的随机安装 id。

**现在有了一个真实的端点。** 已启用的会话会将其批次发送到第一方采集服务 `https://telemetry.codewhale.net/v1/telemetry`，这也是 `telemetry_endpoint` 的出厂默认值。该服务是什么、存储什么、结构上不可能存储什么，都在下面的"端点做什么"一节中说明。

**要想不向任何地方发送任何内容，请关闭遥测**（见"关闭遥测"）。想保持启用但不联系任何人，请设置 `telemetry_endpoint = ""`：此时批次会被追加到你本机的 `$CODEWHALE_HOME/telemetry/dryrun.jsonl`，与服务端本会收到的内容逐字节一致，而且永远不会构造任何 HTTP 客户端。这个文件就是你对照现实审计本文档的方式。

本文档就是 schema。它不是 schema 的摘要：`crates/telemetry` 中的一个测试会解析本文件的字段名，并断言与序列化器实际使用的结构体集合相等，因此文档里有而代码里没有——或代码里有而文档里没有——都会导致构建失败。

本地保存的会话和 `codewhale metrics` 在遥测关闭时仍然可用，不需要账号，也不依赖任何托管的报告服务。关闭遥测不会关闭本地用量回执。

## 关闭遥测

有两个关闭开关，作用各不相同。两者都会彻底停止采集；其中只有一个会擦除任何东西。

```sh
codewhale config set telemetry false     # 选择退出：停止采集并擦除状态
CODEWHALE_TELEMETRY=0 codewhale          # 终止开关：停止采集，不擦除任何内容
codewhale --telemetry false              # 同样的终止开关，仅对单条命令生效
```

**配置文件中的 `telemetry = false` 就是选择退出。** 它是一个底线：`--telemetry true` 和 `CODEWHALE_TELEMETRY=1` 都会输给它，因为一个可能被包装脚本意外撤销的设置算不上设置。它会删除随机安装 id，截断每个已缓冲事件和每条 dry-run 记录，并写入一个 tombstone。追加、身份/状态写入和投递共享同一次擦除的排序锁，因此一旦选择退出返回，就不会有任何退出前写入或 POST 仍在进行中。如果擦除的任何部分失败，tombstone 依然存在，缓冲区无法再排空——擦除失败即按失败即关闭（fail closed）处理。只要该设置仍然生效，每一次后续运行都会重新断言同一个 tombstone，因此它能一直存活；重新开启需要在 `/settings` 中明确修改偏好并更新现有两个隐私记录；之后的新启动才能清除 tombstone。此前缓冲的任何内容都永远不会被发送。

**环境变量和 flag 是终止开关，不是选择退出。** 本次运行期间遥测关闭，不写入任何内容，不发送任何内容——磁盘上也什么都不触碰、不删除。这是刻意的：一个为某条命令设置 `CODEWHALE_TELEMETRY=0` 的 harness 或 agent，绝不能悄悄丢弃机器所有者的安装 id 和 dry-run 记录。如果你想要会擦除的那种，请使用配置文件。

`CODEWHALE_TELEMETRY`（及其别名 `DEEPSEEK_TELEMETRY`）接受 `0`、`1`、`true`、`false`、`yes`、`no`、`on`、`off`、`enabled`、`disabled`。无法识别的值也会解析为 off——终止开关里的拼写错误绝不能解析为"on"。

当遥测已被持久关闭，或本次运行的终止开关生效时，不会显示首次运行提示。该提示也绝不会改写你自己写入的 `telemetry = false`。

仓库本地的 `.codewhale/config.toml` 不能设置 `telemetry` 或 `telemetry_endpoint`，工作区的 `.env` 同样不能设置这两者。别人的仓库无法打开你的遥测，也无法把它指向他们选定的主机。

## 数据存放位置与磁盘占用

所有内容都在 `$CODEWHALE_HOME/telemetry/`（`0700`）之下，每个文件都是 `0600`：

| 文件 | 作用 |
|---|---|
| `buffer.jsonl` | 待处理事件，每行一个 JSON 对象 |
| `buffer.jsonl.lock` | 兄弟排序锁，写入、投递、启动和擦除共享 |
| `dryrun.jsonl` | 端点配置为空时批次的去向 |
| `state.json` | 上次看到的应用版本和上次的 flush 尝试 |
| `install_id.json` | 随机安装 id 及其生成时间 |
| `disabled` | tombstone；存在即表示不会追加或发送任何内容 |

`buffer.jsonl` 和 `dryrun.jsonl` 都是环形缓冲，上限为 512 条记录或 256 KiB，先到先截，最旧的被丢弃。因此整个目录有记录的占用上限为 **512 KiB 加几百字节元数据**。

安装 id 是一个随机的 v4 UUID。它绝不从你的主机名、MAC 地址、`machine-id`、主目录、用户名或可执行文件路径派生——派生的 id 就是设备指纹，重装后依然存在，并在你自己选择退出之后重新识别你。每当 `$CODEWHALE_HOME/telemetry/` 被清空时它都会重新生成（选择退出会自动清空），并且在任何情况下每 90 天轮换一次。

Codewhale 没有恢复出厂设置命令，因此本文档也不会声称有。

## 发送时机与发送去向

持久选择退出、运行级终止开关生效或隐私状态不可读时，不采集也不发送任何内容。TUI 和 `exec` 会话只有一个网络 flush 点：关闭时尝试一次，限时三秒。`config`、`doctor`、`auth` 等短 CLI 命令不会等待这次网络请求：它们记录 `session_end`，在短得多的时限内把该事件封存到本地缓冲后即返回；已配置的端点会在下一次交互式关闭 flush 时发送这些缓冲事件。端点被显式设为空时，会立即定稿本地 dry-run 批次。

没有启动时 flush、会话中途 flush、按回合 flush 或按工具调用 flush。每次网络关闭 flush 都会在执行前立即从磁盘重新解析你的设置，因此从另一个终端写入的 `codewhale config set telemetry false` 会阻止一个已经在运行的会话的 flush。

一次 flush 就是对已解析端点的一次 **`POST`**——默认是 `https://telemetry.codewhale.net/v1/telemetry`。请求携带 `content-type: application/json` 头、`user-agent: codewhale-telemetry/<app_version>` 头，以及批次主体。仅此而已：没有 cookie（HTTP 客户端在构建时就没有可禁用的 cookie jar）、没有重定向（直接拒绝）、没有 `Authorization` 头、没有自定义头、没有查询字符串。响应主体被丢弃不读；只查看状态类别。

`https://` 是必需的。普通的 `http://` 只对回环主机接受，因此你可以把客户端指向自己的一台记录器并直接读取线上格式；没有任何环境变量能覆盖这一拒绝。客户端拒绝的端点会让本次运行的遥测关闭，而不是回退到其他目的地。

任何失败——DNS、连接、TLS、超时、非 2xx——都会丢弃该批次。没有重试、没有退避、没有重新排队。永久离线的机器每次 flush 点最多尝试一次，永远不会积压队列。

---

## 事件 schema

`SCHEMA_VERSION = 3`，`notice_version = 5` 表示已公开说明的默认开启政策，不表示用户已同意或已看到告知。保留 v2 的封闭格式及 `consent_version = 4` 原有明确同意含义，各版本字段不能混用。v1 批次仍只写入第一方 Analytics Engine，绝不转发到 PostHog，也不接受新字段、新界面或 product_usage 事件。除恰好三个有界字符串（`app_version`、`git_sha`、`panic_site`）外，每个字段都是整数、布尔值或**封闭枚举字符串**。这三个字符串各自都有成文规则和一个钉住该规则的测试。**该 schema 中没有自由格式字符串类型，也没有开放键映射。** 正是这一性质使红线 3 可强制执行，而不是停留在愿望层面。

### 批次信封——每次 POST 都会发送

```jsonc
{
  "schema_version": 3,
  "notice_version": 5,
  "sent_at":     "2026-08-03T18:04:11Z",   // RFC3339 UTC，秒级精度
  "install_id":  "3f2a…",                  // uuid v4，每 90 天轮换
  "app_version": "0.9.12",
  "git_sha":     null,                     // 仅带 SHA 标记的构建为非 null
  "surface":     "tui",
  "os":          "macos",
  "arch":        "aarch64",
  "libc":        "none",
  "tty":         true,
  "events":      [ … ]
}
```

| 字段 | 类型 | 来源锚点 | 规则 |
|---|---|---|---|
| `schema_version` | `u32` | `crates/telemetry/src/event.rs` 中的常量 | 任何字段新增/删除/改型时递增。绝不复用。由 golden snapshot 测试钉住。 |
| `notice_version` | `u32` | `crates/telemetry/src/event.rs` 中的政策常量 | 固定为 `5`；标识已公开说明的可退出政策，不是用户同意记录。 |
| `sent_at` | RFC3339 | `chrono::Utc::now()` | 秒级精度。仅按**批次**——事件本身完全不携带时间戳。 |
| `install_id` | uuid v4 | `crates/telemetry/src/envelope.rs` | 随机、绝不派生，每 90 天轮换。见上文"数据存放位置"。 |
| `app_version` | string | `env!("CARGO_PKG_VERSION")`，即 `crates/telemetry/src/lib.rs:112` 处 | 必须匹配 `^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$`。 |
| `git_sha` | string \| null | `option_env!("CODEWHALE_RELEASE_BUILD_SHA")`——一个**新的** rustc-env | 前 12 个十六进制字符。仅当 `codewhale_build_support::release_build_sha` 按此优先级在 `CODEWHALE_BUILD_SHA`、旧版 `DEEPSEEK_BUILD_SHA` 或 `GITHUB_SHA` 中看到有效的完整 SHA 时才发出。所有未打 SHA 标记的构建一律为 `null`，且不做任何形式的运行时查找。**绝不**是 `CODEWHALE_BUILD_COMMIT`——那会回退到 `git_commit`，是构建者的私有 HEAD。**绝不**是 `Thread.git_sha`（`crates/state/src/lib.rs:93`）——那是用户工作区的提交，是一条红线，只隔一个名字。 |
| `surface` | enum | 由发出批次的客户端明确设置 | `tui \| exec \| cli \| app-server \| mcp-server \| serve \| website \| web-app \| desktop \| control-plane \| vscode-extension`。运行时指标沿用其现有的采集器。浏览器产品计数使用同一个封闭信封。声明某个 surface 不代表其客户端已经部署。 |
| `os` | enum | `std::env::consts::OS`，即 `crates/cli/src/update.rs:41` 处 | allowlist：`linux \| macos \| windows \| freebsd \| android \| other`。 |
| `arch` | enum | `std::env::consts::ARCH` | `x86_64 \| aarch64 \| other`。 |
| `libc` | enum | `cfg!(target_env)`——**编译期** | `gnu \| musl \| none`。运行时检测会读取发行版厂商字符串；编译期免费且不泄露任何内容。 |
| `tty` | bool | `std::io::IsTerminal`，即 `crates/telemetry/src/envelope.rs:196` 处 | `stdin().is_terminal() && stdout().is_terminal()`。 |
| `events` | array | 被排空的缓冲区 | 每个元素都只能是下面六种事件之一，没有其他。每批次上限为 200 个事件或 64 KiB；超出任一上限的批次会把剩余部分留到下一次 flush。 |

**`os_major` 不会被收集。** 读取它需要两个平台上的不安全 FFI 外加第三个平台上的文件解析器，而这正是那个以"小到足以审计"为全部价值的 crate——而 `os`、`arch`、`libc` 是免费的，并且能回答平台问题。如果存储的数据将来显示 OS 版本切分正是分诊所缺的，可以重新考虑；光凭直觉不是那样的证据。

### 哪些界面发送

所有运行界面使用同一个 `decide` 与发送前复查：要求可读的隐私状态、可用的主目录、有效端点，并且没有持久退出或运行级终止开关；新用户缺少偏好时默认开启。TUI 提供告知和设置入口，无头命令不会代替用户记录同意。现有运行事件和计数器不变，不增加第二套采集器。

### 嵌入方声明它启动的服务

启动 `codewhale serve` 的客户端可以声明自己，使其会话不会被记成谁也不是的匿名 `serve` 流量。它在启动服务前把 `CODEWHALE_TELEMETRY_SURFACE` 设为 `surface` 白名单中的一个值；编辑器扩展使用 `vscode-extension`。

该读取有三条刻意的约束：

- **仅对 `serve` 生效。** 该变量会被后代进程继承，而 agent 执行 shell 命令时正是在用这套环境运行 `codewhale` 后代。只有"本身就是嵌入方那个服务"的分支会读取它，因此嵌套的 `codewhale exec` 不会把自己报成嵌入方——那既是错的，又看不出来。
- **拒绝 `tui`**，而 `serve --mcp` 仍然报 `mcp-server`：没有人会替用户启动交互式终端界面，服务也不会因为嵌入方声明就变成 MCP 服务。
- **无法识别的名称被丢弃，而不是被修正。** 白名单之外的值回退为 `serve`，既不导致启动失败，也不会被猜成一个相近的值；该字段只是一个标签，默认值才是诚实的那个。

这改变的是嵌入方会话的**标签**，而不改变是否可以收集：服务自行从同一份持久配置、同一份 setup 状态和同一个运行级终止开关 `CODEWHALE_TELEMETRY` 解析该决定。嵌入方无法在用户已关闭时开启收集；由于共用同一主目录和同一个安装身份，在任一客户端做出的**持久**退出即对本机所有客户端的退出。

扩展客户端是现成的例子：它只有一个开关，在用户关闭上报时传入 `CODEWHALE_TELEMETRY=0`。它自身没有载荷构造器、没有缓冲、也没有传输层——因此它的会话共用主目录的 `install_id`，而不是另建一个；一次安装在由该 ID 派生的任何计数里都是一个安装。

由于该开关是运行级而非持久退出，关闭它只会停掉这个扩展启动的引擎，不会动共享的缓冲与身份：那些属于本机上的每一个界面，而不是某一个窗口；清除它们会静默改变用户终端会话的行为。若要在全机停止并清空上报，执行一次 `codewhale config set telemetry false` 即可。

网站和应用使用 `product_usage`：告知版本 `5`，浏览器本地随机 v4 ID 每 90 天轮换；`os = other`、`arch = other`、`libc = none`、`tty = false`、`git_sha = null`，不读取浏览器指纹。同源代理仅向第一方采集端转发封闭 JSON，不转发入站 cookie、请求头、URL 或身份。未配置端点时不发送。浏览器退出统计会清除待发送计数和本地 ID；关闭期间的操作不会补发。产品使用统计设置放在应用和运行时；营销网站的隐私说明及退出入口放在隐私页面。

### 事件：install_or_upgrade

当 `state.json` 的 `last_version` 与 `app_version` 不同时发送一次。

```jsonc
{ "event": "install_or_upgrade", "kind": "upgrade", "previous_version": "0.9.3" }
```

| 字段 | 类型 | 来源 | 规则 |
|---|---|---|---|
| `kind` | enum | 派生 | `install`（无先前记录）\| `upgrade` \| `downgrade`。 |
| `previous_version` | string \| null | **仅** `$CODEWHALE_HOME/telemetry/state.json` | 与 `app_version` 相同的正则。绝不从会话历史或配置 mtime 派生——那些文件有不同的隐私契约。 |

### 事件：session_start

```jsonc
{ "event": "session_start", "source": "interactive" }
```

`source` 是 `SessionSource`（`crates/state/src/lib.rs:34-41`），由 `session_source_to_str`（`:1909-1917`）字符串化：`interactive | resume | fork | api | unknown`。

### 事件：session_end

主力事件。会话积累的一切都在这一个事件里发出，只发一次。

```jsonc
{
  "event": "session_end",
  "duration_bucket": "1m_10m",
  "exit_class": "clean",
  "cold_start_bucket": "250_1000",
  "providers": ["deepseek", "custom"],
  "counters": { "turns": 14, "tool_calls": 61, "fleet_dispatch": 0, "workflow_run": 0,
                "subagent_spawn": 2, "mcp_server_connected": 0, "memory_search": 0,
                "approval_modal_shown": 0, "approval_auto_allowed": 0,
                "command_palette_open": 3 },
  "errors":   { "auth_preflight_failed": 0, "provider_http_4xx": 0, "provider_http_5xx": 1,
                "tool_denied_by_policy": 0, "tool_timeout": 0, "network_error": 0 },
  "turn_wall": { "lt_5s": 9, "5_30s": 4, "30_120s": 1, "gte_120s": 0 }
}
```

**`counters` 和 `errors` 是 `#[derive(Serialize)]` 的具名 `u32` 字段结构体，不是映射。** 每个字段都会被序列化，包括零值。键集合由编译器封闭：新增计数器需要编辑 `crates/telemetry/src/event.rs`，文档匹配测试就在那里。

**`duration_bucket`** ——来自 `app.session_started_at`（`crates/tui/src/tui/app.rs:1889`）的 `chrono` 差值。半开区间，单位秒：`lt_1m`（`d < 60`）、`1m_10m`（`60 ≤ d < 600`）、`10m_60m`（`600 ≤ d < 3600`）、`gt_60m`（`d ≥ 3600`）。

**`exit_class`** —— `clean | signal | panic | error`。**来自显式的 `AtomicU8`，绝不来自退出码。** `RunTerminationReason::Canceled` 映射到退出码 130（`crates/tui/src/core/runtime_contract/termination.rs:53`），与信号任务使用同一个值（`crates/tui/src/lib.rs:682`，128+SIGINT），因此基于退出码的推导会把每次 Esc 取消的回合都报成信号。这个 atomic 由 panic hook（`crates/tui/src/lib.rs:1582`）、信号任务（`:678-696`，在 `std::process::exit` 之前）以及干净路径上的 `RunTerminationReason::is_success()`（`crates/tui/src/core/runtime_contract/termination.rs:44-46`）设置——其他情况为 `error`。**不要**使用 `exec_failure_exit_code`（`crates/tui/src/lib.rs:10432`）：它只知道 `{75, 1}`，会把需要审批的退出码（3）报成普通失败。

**`cold_start_bucket`** ——来自 `startup_trace::elapsed_ms()`，它直接读取 `PROCESS_START`，并且独立于启动摘要的缓冲区清空（`crates/runtime/src/startup_trace.rs:39-45`）。边界：`lt_250`、`250_1000`、`1000_3000`、`gte_3000`。非 TUI surface 上缺席。

**`providers`** ——`ProviderKind::as_str()`（`crates/config/src/provider_kind.rs:295`，来自封闭枚举的 `&'static str`；`Custom` 产生字面量 `"custom"`）的排序、去重数组。**API 按值接收 `ProviderKind`，绝不接收 `&str`。** 不要调用 `ProviderKind::parse` 或 `parse_config_identity`（`:300`、`:330`）——那些用于配置表解析。**不要读取** `provider_identity_for_persistence()`（`crates/tui/src/tui/app.rs:5049`）、`provider_id_for_persistence()`（`:5058`）、`ExecStreamMeta.provider_id`（`crates/tui/src/lib.rs:10220`）或 `PlannedTurnRoute.effective_provider_label`（`crates/tui/src/turn_route_plan.rs:189-193`）——当路由是 Custom 时，这四个都会返回客户自己的 `[providers.<name>]` 表键。这是该功能最可能的泄露点：它离天然接缝只差一个字段，而且 `/status` 已经会打印它（`crates/tui/src/commands/groups/config/status.rs:24-28`）。**任何提供商都绝不发送模型 id**——`crates/runtime/src/safe_label.rs:11-15` 记录了一个事实：模型 id 可以是路径、URL 或本身就是凭据的部署 id。

**`counters`** ——封闭字段集。每次递增都发生在**调用点**，绝不在条件进入的处理器内部：

| 字段 | 来源锚点 |
|---|---|
| `turns` | `crates/tui/src/tui/ui/event_loop.rs` 中的 `run_event_loop`，紧接在它调用 `execute_turn_end_observer_hook` 之前。绝不在该 hook 内部：它的第一条语句是 `if !app.hooks.has_hooks_for_event(HookEvent::TurnEnd) { return Ok(()); }`（`crates/tui/src/tui/ui/observer_hooks.rs`），而自然的未来优化会把该检查提升到调用点，从而悄悄把所有没有 hooks 的用户的计数器归零。 |
| `tool_calls` | `crates/tui/src/core/engine/tool_execution.rs` 中的 `execute_tool_with_lock`——与 surface 无关，exec 和 CLI 也会触发 |
| `fleet_dispatch` | `crates/tui/src/fleet/manager.rs` 中的 `create_queued_run_with_descriptor`——单一漏斗，`create_run` 和 `create_queued_run` 都落入其中；在任一调用方计数都会使普通的 `fleet run` 被重复计数。 |
| `workflow_run` | 在 `WorkflowTool::execute`（`crates/tui/src/tools/workflow/mod.rs`）中、仅当 `parse_workflow_action` 返回 `Ok(WorkflowAction)` 之后递增，绝不从 `input["action"]` 计数。`WorkflowTool::input_schema` 中的 JSON Schema `enum` 是发布*给模型*的——是声明，不是守卫；真正的解析还接受 `spawn\|wait\|list\|inspect\|stop\|abort`，其拒绝分支（`Invalid workflow action '…'`）会原样嵌入模型字符串，因此被拒绝的 action 永远不会被计数。 |
| `subagent_spawn` | `crates/tui/src/tui/ui/apply.rs` 中的 `apply_agent_spawned_status_and_observer` |
| `mcp_server_connected` | 在 `snapshot_from_config`（`crates/tui/src/mcp.rs`）中，服务器快照的 `.connected` 为 true 时递增；绝不统计 `name`、`command_or_url` 或 `error`——服务器名是用户自选的，往往是内部基础设施 |
| `memory_search` | `tool_name == "memory_search"`（在 `crates/tui/src/tools/native_memory.rs` 中注册的工具），在同一个 `execute_tool_with_lock` 瓶颈点计数 |
| `approval_modal_shown` | `run_event_loop` 的 `Event::ApprovalRequired` 分支（`crates/tui/src/tui/ui/event_loop.rs`；该事件定义于 `crates/tui/src/core/events.rs`） |
| `approval_auto_allowed` | `crates/tui/src/core/engine.rs` 中的 `tool_ask_rule_decision_for_context`。只计数。绝不统计 `matched_rule`、`reason()`、命令或 argv——`auto_allow` 模式是用户编写的命令字符串（`crates/execpolicy/src/command_safety.rs:35/309`） |
| `command_palette_open` | `run_event_loop` 中的命令面板按键路径（`crates/tui/src/tui/ui/event_loop.rs`）以及 `crates/tui/src/tui/mouse_ui.rs` 中的 `handle_context_menu_action` |

**`errors`** ——封闭字段集。每个值都是**变体判别值**，绝不是 `err.to_string()`：

| 字段 | 来源锚点 |
|---|---|
| `auth_preflight_failed` | `CredentialReadiness`（`crates/workflow/src/fleet_preflight.rs:37-58`）/ `ProviderAuthClass`（`crates/tui/src/provider_readiness.rs:32`）的判别值。只取判别值——`Missing { detail }` 携带自由文本 |
| `provider_http_4xx` | `status.as_u16() / 100 == 4`，在 `crates/tui/src/client/chat.rs:595` 和 `:673` 处、**在** `bail!` **之前**捕获。每字段一行，因为文档匹配测试会逐字段读取此表 |
| `provider_http_5xx` | `status.as_u16() / 100 == 5`，同样的捕获点 |
| `tool_denied_by_policy` | `crates/tui/src/core/engine/tool_execution.rs:512-531` 处 8 变体匹配的 `permission_denied` 分支 |
| `tool_timeout` | 同一匹配的 `timeout` 分支 |
| `network_error` | `retry_reason_label_and_human()` 的 `&'static str` 一半，`crates/tui/src/client.rs:2659` |

为什么只要判别值：`ToolError::PathEscape` 的 `Display` *就是*一个绝对路径（`crates/tools/src/lib.rs:61`）；`fim.rs:48-50` 的 `Display` *就是*模型发出的字面源码片段；`secrets/src/lib.rs:50` 的 `Display` 携带密钥库的绝对路径；每个 `LlmError` 变体都原样携带提供商的原始 HTTP 主体（`crates/tui/src/llm_client/mod.rs:327`），而内容过滤器的 400 通常会回显提示词。

**`turn_wall`** ——按会话的计数直方图，绝不是按回合的事件。`lt_5s`、`5_30s`、`30_120s`、`gte_120s`。由 `run_event_loop` 中紧挨 `turns` 递增处的 `observe_turn_secs` 记录，那里已经手握本回合耗时。

### 事件：panic

由 panic hook **同步**追加，因为 `session_end` 可能永远写不出来。

```jsonc
{ "event": "panic", "site": "crates/tui/src/lib.rs:1582:5" }
```

`site` 来自 `panic_info.location()`（`crates/tui/src/lib.rs:1597-1600`）或 `Location::caller()`（`crates/tui/src/utils.rs:523`）。**allowlist 缩减，不是可选的：** 仅当 `file()` 以 `crates/` 开头时才原样发送；否则发送字面量 `"<dep>"`。必须匹配 `^crates/[A-Za-z0-9_/.-]+\.rs:\d+:\d+$` 或 `^<dep>$`。本仓库没有 `--remap-path-prefix`（没有 `.cargo/config.toml`；`Cargo.toml:69-74` 只设置 `lto`/`strip`/`codegen-units`），因此注册表依赖内部的 panic 会得到 `/Users/<builder>/.cargo/registry/src/…/ratatui-0.29.0/src/…`——**构建机器的用户名**，从每个用户的二进制里发送出去。

**panic 消息绝不发送。** `crates/tui/src/lib.rs:1590-1596` 处的 hook 从 payload 构建 `msg`；遥测绝不能读取它。切片（slicing）panic 会嵌入正在被切片的整个字符串，而本代码库在几十处切片用户和模型文本。

### 事件：product_usage

只统计明确的产品交互，所有字段均为饱和 `u32`，包括零值。不得包含页面路径、URL、来源页、搜索词、账号或会话 ID、文本或操作时间戳。页面退出或隐藏时汇总发送，不逐次发送操作。

```jsonc
{
  "event": "product_usage",
  "counters": {
    "page_view": 1,
    "docs_view": 0,
    "install_copy": 0,
    "download": 0,
    "signup": 0,
    "login": 0,
    "session_create": 0,
    "session_resume": 0,
    "turn_submit": 0,
    "turn_complete": 0,
    "settings_open": 0,
    "integration_connect": 0,
    "error_shown": 0
  }
}
```

`page_view` 是页面访问次数，`docs_view` 是文档访问次数，`install_copy` 和 `download` 是安装操作次数。`signup`、`login` 只统计成功操作，不含身份。`session_create`、`session_resume`、`turn_submit`、`turn_complete` 统计相应操作。`settings_open`、`integration_connect`、`error_shown` 不包含具体设置、集成名称、错误正文或工作内容。零值不代表某功能可用或已被使用。

### 事件：operations_summary

复用控制平面现有信号记录，汇总匿名服务健康状况。仅允许下列六个饱和 `u32` 字段，且界面必须为 `control-plane`。此路径要求独立于用户分析选择的明确运维投递/处理方配置。每批使用新的随机 v4 `install_id`，不关联用户、账号或会话。不允许逐请求记录、路由、请求摘要、ID、错误正文、追踪或时间序列。`requests` 和 `errors` 统计请求与失败；`duration_ms_total` 和 `duration_ms_max` 是汇总延迟；`probes` 和 `probes_failed` 统计健康探测。

```jsonc
{
  "event": "operations_summary",
  "requests": 10,
  "errors": 1,
  "duration_ms_total": 1500,
  "duration_ms_max": 300,
  "probes": 2,
  "probes_failed": 0
}
```

### 端点做什么——发布门槛，而非脚注

本节曾是配置任何非回环端点的门槛。端点现在默认已配置，因此这是对已存在服务的描述，而不是对可能存在的服务的承诺。

**它是什么。** `https://telemetry.codewhale.net/v1/telemetry` ——一个名为 `codewhale-telemetry-ingest` 的 Cloudflare Worker，其完整源码就在本仓库的 [`telemetry-ingest/`](../../telemetry-ingest/) 中。它是唯一的 schema 与存储权威；浏览器应用可使用只转发主体的同源代理。没有遥测队列或第二套运行时采集器。它只写：Worker 中没有任何东西能回读已存储的内容，查询通过 Cloudflare 的 SQL API、以所有者的令牌带外进行。主机名刻意自描述，因此任何检查自己网络流量的人光看名字就能知道它是什么。

**它存储什么。** 本文档中的一切，仅此而已，存放在 Workers Analytics Engine——每个事件一行。`telemetry-ingest/src/schema.ts` 中的校验器是一个**封闭**字段集：批次任意位置的未知键都会以 `400` 拒绝整个批次。未来某个客户端 bug 开始附带路径、提示词或提供商表名时，会被服务器拒绝，而不是被悄悄存储。`telemetry-ingest/test/schema-doc.test.ts` 会从*本文件*中解析出字段名和枚举拼写，并断言与校验器的集合相等；`telemetry-ingest/test/ingest.test.ts` 会发布 Rust 客户端自己钉住的 golden 批次，并断言它被逐字节接受——因此本文档、客户端和端点不会在没有红色测试的情况下彼此漂移。

批次在**采集时剥离 IP**。不存储、不记录 IP，也不与 `install_id` 关联——这是结构性的，而不是任何人都能翻转的设置。Analytics Engine 的一行恰好是 `_sample_interval`、`blob1`–`blob20`、`dataset`、`double1`–`double20`、`index1` 和 `timestamp`。这些列中的每一列都由 Worker 自己的 `writeDataPoint` 调用写入；没有隐式列，因此**没有 IP、国家或地理列**——即使代码想放也没有任何槽位可以占据。而且代码不可能想：

- handler 恰好读取**两个**请求头——`content-type` 和 `content-length`。其他任何东西，永远不会。
- 它从不触碰请求的 `cf` 属性，因此国家、colo、城市、地区、ASN、时区、坐标永远不在范围内。
- 构建每一存储行的函数根本看不到请求；它的输入类型是校验过的批次主体。
- 什么都不记日志。`invocation_logs` 在 `wrangler.jsonc` 中是关闭的——Cloudflare 将这类日志描述为"在调用上下文中携带 Cloudflare 可用的信息增强"，而这正是本服务不会保留的那类自动按请求记录——源码中也没有任何 `console.*` 调用。
- 限流以校验过主体中的 `install_id` 为键，绝不基于网络地址。按 IP 键控的限流器意味着这个 Worker 要处理 IP。它是更弱的限流器，也是正确的取舍。
- `telemetry-ingest/test/no-ip.test.ts` 将已发布的源码作为文本读取，如果出现任何地址或地理名称、读取的头集合增长到两个以上、新增 `console.*` 调用，或构造带主体的 `Response`，构建就会失败。

**保留期：三个月。** 这是 Analytics Engine 的固定窗口，不可配置，因此它是上限而非策略——没有任何设置能让它更长。

**可选 PostHog 数据处理方。** 第一方存储完成后，明确配置的采集服务可把已验证的 schema-v3 / notice-v5 批次及保留原有含义的 schema-v2 / consent-v4 批次发往 [PostHog 批量采集 API](https://posthog.com/docs/api/capture)。只允许 `https://us.i.posthog.com` 和 `https://eu.i.posthog.com`；v1 批次永不进入此路径。PostHog 接收相同的有限字段，以批次时间作为事件时间，以 `codewhale:<install_id>` 作为匿名 `distinct_id`；事件名分别为 `codewhale_install_or_upgrade`、`codewhale_session_start`、`codewhale_session_end`、`codewhale_panic`、`codewhale_product_usage` 或 `codewhale_operations_summary`。固定设置 `$process_person_profile = false`、`$geoip_disable = true`、`$ip = null`。不转发请求元数据、身份识别、自动采集、会话回放、广告或工作内容，不添加 SDK。全新的服务端请求不携带用户 cookie 或身份验证头，不跟随重定向，限时 1.5 秒，不重试、不记录日志；处理方失败不改变已成功的第一方响应。

PostHog 项目的保留期限和隐私设置是单独的部署配置，启用前必须复核。Analytics Engine 的三个月上限不能代表 PostHog 的保留期限。本地退出统计不会删除已经发送到处理方的数据；客户端没有远程删除 API。

**边缘网络出口还需预发布验证。** [Cloudflare 文档](https://developers.cloudflare.com/fundamentals/reference/http-headers/#cf-connecting-ip-in-worker-subrequests)说明，Worker 向非 Cloudflare 区域发出的子请求可能被平台添加客户端 IP 请求头。新建 JavaScript 请求头和本地 fetch 模拟测试无法证明平台不会添加这些头。除主机和令牌外，`POSTHOG_IP_SAFE_EGRESS_VERIFIED="true"` 是单独的运维前提；在实际部署和所选区域目的地的验证证据确认所有接收头（包括 `CF-Connecting-IP`、`X-Forwarded-For`、`X-Real-IP`）均不含原始客户端 IP 之前，必须保持未设置。该标志本身不删除平台头。出口变化后需重新验证；若无法满足要求，保持转发关闭，或将发送环节移到与入站请求上下文分离的第一方环境。此源码变更不构成任何线上出口验证。

**每个响应都是带空主体的裸状态码**——`204` 接受、`400` schema 违规、`404`/`405` 路径或方法错误、`413` 过大、`415` 内容类型错误、`429` 被限流、`500` 内部错误。端点无法回显它收到或持有的内容，而且由于客户端会在任何非 2xx 时丢弃批次，拒绝对你而言在构造上就是不可见的，服务器错误也永远不会表现为客户端可见的失败。

`install_id` 每 90 天在客户端轮换（`install_id.json` 中的 `rotated_at`），因此没有任何单一标识符跨越很长的历史。这牺牲了纵向准确性，文档也如实说明：**任何由 `install_id` 推导出的计数都不是用户数。** 它是某个窗口内不同机器安装数的下界，并且会在一次轮换中少算一个回访用户。

**关闭它会删除本地保留的内容，而不是已经发送的内容。** `codewhale config set telemetry false` 会擦除你机器上的安装 id、缓冲区和 dry-run 记录，并停止后续一切。端点已经接受的行只由现在已消失的轮换随机 id 键控；其中第一方的行随三个月窗口一起过期。如果启用了 PostHog 处理，已投递给处理方的记录遵循该项目自行配置的保留期限。客户端没有远程删除 API，也不声称本地退出统计会擦除已投递的记录。

### 所有者回读的内容——观察到的活跃安装数

从这些数据推导出的唯一产品指标是**观察到的活跃安装数（observed active installs）**：在一个 UTC 日内产生 `session_start` 事件的不同轮换匿名安装 id 的数量。这就是全部定义。它不是人数、不是账户数、也不是安装总数——id 按安装存在，每 90 天轮换，选择退出时被删除，因此任何由它推导出的数字都不可能是这些。

所有者的确切命令已签入仓库，因此例行查询是可审阅的代码，而不是从聊天里粘贴的 SQL：

```sh
cd telemetry-ingest
CF_ACCOUNT_ID=... CF_API_TOKEN=... npm run report:active-installs
```

它打印每日序列、完整 UTC 日上的 7 天趋势、最新摄入事件的时效性，以及——连同数字，在文本和 `--json` 输出中都有的——覆盖率注意事项：比遥测功能更老的客户端、选择退出的安装、不发送的环境（终止开关、fleet worker、离线关机、被丢弃的 flush）都是不可见的，因此每个计数都是下界；又因为 id 会轮换，周与周的比较不是留存指标。报告的读取路径本身也经过测试：安装 id 只出现在聚合内部，不选择任何 payload 列，措辞也绝不会漂移成把结果称为用户。（`npm run report:dau` 仍是同一报告的兼容别名。）

### 绝不收集的内容——公开红线清单

提示词；补全；工具参数；diff；补丁；文件内容；文件名；绝对或相对路径；git remote；仓库名；分支名；工作区提交 SHA；记忆条目；聊天历史；API 密钥、令牌、cookie 或 `Authorization` 头（包括任何断言密钥存在的布尔值）；任何种类的模型 id；自定义提供商表名；MCP 服务器名、命令或 URL；审批规则文本；错误消息主体；panic 消息文本；按事件的时间戳；按键；剪贴板；截图；麦克风；摄像头；位置；以及任何第三方广告或分析 SDK——运行时二进制中没有，也不得添加。

给实现者的两个具名陷阱。`crates/state/src/lib.rs` 在线程表上持久化 `git_sha`、`git_branch`、`git_origin_url`、`cwd` 和 `path`（`:93, :399, :653`）：一个接受 `Thread` 或 `ThreadMeta` 并 `derive(Serialize)` 的 payload 构建器一行就违反契约。**绝不在现有状态类型上派生 `Serialize`**——从零开始、用显式字段构建每个遥测结构体。而 `crates/core/src/lib.rs:1389-1398` 是整棵树中 `telemetry` 一词与 `prompt`、`base_url`、`has_api_key` 同处一个 JSON 对象的唯一位置。它是某人会复制的那一个对象。不要复制。

---
