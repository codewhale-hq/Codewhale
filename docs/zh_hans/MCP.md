# MCP（外部工具服务器）

> 本文翻译自英文版 [MCP.md](../MCP.md)，与英文源文件同步于 2026-09-29。

在终端中，`/mcp`（也可用 `/mcps`）会打开 **Extensions → MCP**。按 Enter 会对所选服务器执行恢复操作，或打开只读详情。清单为空时会提供服务器建议；浏览这些建议不会安装任何内容。`/mcp status`、`/mcp doctor`、`/mcp login`、`/mcp add` 等显式子命令的行为保持不变。

codewhale 可以通过 MCP（Model Context Protocol，模型上下文协议）加载额外工具。MCP 服务器可以是 TUI 启动的本地 stdio 进程，也可以是使用 Streamable HTTP（带传统 SSE 回退）的远程基于 URL 的服务器。

浏览说明：
- `Web` 是规范的、延迟加载的内置浏览工具；在网络策略允许时，它提供 `search`、`fetch` 和 `wait` 操作。
- `web_search`、`fetch_url` 和 `wait_for_dev_server` 是隐藏的仅回放别名。新的提示和集成应使用 `Web`。

服务器模式说明：
- `codewhale serve --mcp` 运行 MCP stdio 服务器。
- `codewhale serve --http` 运行运行时 HTTP/SSE API（独立模式）。
- `codewhale mcp-server` 是同一个统一运行时上等价的 stdio 入口。

0.10.1 移除了旧的子服务器聚合代理。`mcp-server` 现在与 `serve --mcp`
使用同一个原生工具服务器，不再启动旧的 `mcp.server_definitions` 列表。
已保存的定义会保留。请在 `mcp.json` 中为现有 MCP 客户端配置外部服务器，
或让外部客户端直接连接这些服务器。原生服务器默认不暴露写入工具；只有
操作员在服务器配置中明确允许后，这些工具才会开放。

## 设置向导与手动 MCP 设置（#3407）

`/setup` 中心包含一个可选的 **Tools and MCP** 步骤。该步骤仅用于发现/就绪检查：

| 向导可以做的 | 仍然需要手动/显式操作 |
| --- | --- |
| 将已配置的服务器显示为 `healthy` / `needs_config` / `off` | 启动或连接 MCP 服务器 |
| 报告配置路径是否存在（全局 + 项目） | 写入或编辑 `mcp.json` 内容 |
| 安全的静态健康探测（缺少 command/url、损坏的绝对路径、缺少 bearer 环境变量） | `codewhale mcp validate`、实时连接、OAuth 登录 |
| 指向安全的入门路径（`/mcp`、`codewhale mcp init`、`codewhale doctor`） | 安装社区技能、信任技能、启用插件 |
| 共享来自同一 skill/MCP 适配器的 Hotbar 来源计数（#3399） | 绑定 Hotbar 槽位（Hotbar 步骤 / `H`） |
| 记录可选的/`needs_action` setup_state，不阻塞首次运行 | 任何会生成进程或安装包的操作 |

空清单**不是**错误：首次运行的用户会看到"尚未配置任何内容，这没关系。"失败或不完整的已配置服务器会以 `needs_config` 状态显示，并附带可操作的提示，绝不会阻塞设置完成。枚举过程除静态探测外，绝不执行 MCP/插件命令。摘要会隐去命令、参数、环境变量、请求头和令牌。

`codewhale doctor` 以相同的可选表面意图（路径、计数、静态检查）报告 MCP/技能/工具/插件的健康状态，使向导与 doctor 保持一致。

## 插件贡献的 MCP

通过审查的本地插件包可以贡献 MCP 服务器，而无需创建第二套传输或审批系统。这些服务器使用本文档记载的同一条 MCP 管理器、工具审批、资源、提示、超时和网络策略路径，并以命名空间化的 `<plugin>-<server>` 身份出现。

插件包边界有意比用户编写的 `mcp.json` 更严格：未知字段和模糊的传输方式默认失败关闭；stdio 环境变量值必须是精确的环境来源引用；远程字面量请求头和含机密信息的 URL 会被拒绝；声明的网络主机必须与规范化后的端点主机集合完全匹配；重定向保持在已审查的源站内。已审查的插件远程端点还会完全绕过环境中的 HTTP 代理配置；代理凭据和代理可见流量不属于 v1 审查范围。插件审查会披露本地主机用户权限、结构化 argv、环境来源、端点、认证来源名称、作用域和工具过滤器，但不会读取或打印机密值。

信任（Trust）会暂存已审查的内容，但不会启用它。启用操作会把该暂存快照附加到当前工作区的 MCP 池。禁用、撤销以及其他跨进程的代际变更会移除目录条目、取消进行中的操作，并终止插件的 stdio 子进程。在每次分派/目录边界之前，源码或暂存树的漂移都会被完全重新验证，并让下一个边界失败关闭；v0.9.1 不会在已经运行的调用期间持续对可变树做哈希，因此不承诺由漂移触发的调用中途取消。MCP 订阅不会通过插件包暴露。完整的生命周期契约请参阅[插件包](./PLUGIN_BUNDLES.md)。

## MCP 配置引导

在解析出的 MCP 路径处创建一份入门 MCP 配置：

```bash
codewhale mcp init
```

`codewhale setup --mcp` 会在技能设置的同时执行相同的 MCP 引导。

常用管理命令：

```bash
codewhale mcp list
codewhale mcp tools [server]
codewhale mcp add <name> --command "<cmd>" --arg "<arg>"
codewhale mcp add <name> --url "http://localhost:3000/mcp"
codewhale mcp add <name> --url "https://example.com/mcp" --bearer-token-env-var MCP_TOKEN
codewhale mcp login <name>
codewhale mcp logout <name>
codewhale mcp enable <name>
codewhale mcp disable <name>
codewhale mcp remove <name>
codewhale mcp validate
```

`codewhale mcp logout <name>`（以及 `/mcp logout`）只会清除本地存储的 OAuth 凭据——提供商可能仍保留其既有授权。下一次登录会强制显示授权确认页，因此可以更换所授权的账号/工作区；若要在远端彻底撤销授权，请在提供商的账号设置中撤销该应用。

## TUI 内管理器

在交互式 TUI 中，`/mcp` 会为解析出的 MCP 配置路径打开一个紧凑的管理器。它显示每个已配置的服务器、该服务器是启用还是禁用、其传输方式、命令或 URL、超时值、连接错误，以及运行发现后得到的工具/资源/提示。

TUI 内支持的操作：

```text
/mcp init
/mcp init --force
/mcp import
/mcp recommendations
/mcp add recommended <id>
/mcp add stdio <name> <command> [args...]
/mcp add http <name> <url>
/mcp login <name> [--scope scope]
/mcp logout <name>
/mcp enable <name>
/mcp disable <name>
/mcp remove <name>
/mcp validate
/mcp reload
```

### 推荐插件与配套集成

`/mcp recommendations` 是 Codewhale 原生的精选建议界面。条目被描述为产品插件，带有其组件类型和来源，但 `/mcp add recommended <id>` 仍然只写入指定的 MCP 服务器组件。查看推荐绝不会获取、安装、信任或启用任何内容。添加一条推荐会写入配置；服务器只有在显式执行 `/mcp restart` 之后才会首次启动。

v0.9.10 的产品建议使用这些经过审查的固定版本定义。Plugins 视图是产品/安装界面；MCP、Skills 和沙箱适配器是透明的组件类型，它们各自的标签页仍然是可操作和可诊断的界面：

| 插件 | 组件 | 固定版本定义 | 来源与成熟度 | 安装边界 |
| --- | --- | --- | --- | --- |
| Chrome DevTools | MCP 服务器（stdio） | `npx -y chrome-devtools-mcp@1.7.0`（Windows 上为 `npx.cmd`） | [ChromeDevTools 官方项目](https://github.com/ChromeDevTools/chrome-devtools-mcp) | 用户重启 MCP 时，npm 可能会下载固定版本的包。 |
| Playwright | MCP 服务器（stdio） | `npx -y @playwright/mcp@0.0.79 --isolated`（Windows 上为 `npx.cmd`） | [微软官方项目](https://github.com/microsoft/playwright-mcp) | `--isolated` 会启动全新的浏览器配置文件；npm 只会在显式重启后才下载固定版本的包。 |
| Computer Use | 第一方插件（MCP + skill） | 随二进制内置为 `computer-use` 插件 | Codewhale；通过 `/plugin` 或扩展市场启用 | 这是 Codewhale 唯一推荐的计算机操作集成。此处不再列出第三方桌面控制 MCP。 |
| Browser Use | Skill 加单独安装的 Python 运行时 | Skill/运行时版本 `0.13.8` | [browser-use 官方项目](https://github.com/browser-use/browser-use) | 可选配套：不是 MCP 服务器。Codewhale 不会自动运行上游的 Skill 安装器，也不会安装其浏览器/运行时依赖。 |
| Anthropic Sandbox Runtime | 沙箱适配器配套 | `@anthropic-ai/sandbox-runtime@0.0.73` | [anthropic-experimental 官方项目](https://github.com/anthropic-experimental/sandbox-runtime)；测试版 | v0.9.10 中仅文档说明的适配器候选：不是 MCP 服务器，也不是活跃的 Codewhale 插件适配器。它不会取代 Codewhale 的沙箱策略。 |

[Container Use](https://github.com/dagger/container-use) 仍然是一个带有 MCP 服务器组件（`container-use stdio`）的额外实验性建议。该二进制必须单独安装；`/mcp add recommended container-use` 只写入配置，Codewhale 绝不会下载它。

这种呈现方式遵循了本地 Grokbuild 扩展视图中的同样有用的边界（一个产品插件可以暴露 MCP 或 Skill 组件，同时组件标签页仍可检查）、Kimi 市场的显式显示名称/层级/来源字段，以及 Codex 市场的显式来源和安装策略字段。Codewhale 保持其更严格的规则：来源和外部策略只是显示元数据，绝不会被继承为信任或自动安装。完整的插件包和市场语义请参阅[插件包](./PLUGIN_BUNDLES.md)。

`/mcp validate`（别名 `/mcp doctor`）只为了 UI 发现而重新连接：它刷新你在分页器中看到的管理器快照，而不是模型拿到的目录。

`/mcp reload`（别名 `/mcp reconnect`、`/mcp restart`）是热重载路径。它会重新读取 MCP 配置来源，并通过引擎持有的连接池重新连接，因此重建后的目录正是下一个模型回合所用的目录——无需重启 TUI。从 TUI 中做出的配置修改会立即写入，管理器会把快照标记为需要重载，直到你运行它；重载失败会保留之前活跃的连接池不变，并如实说明。

无头界面是例外：`ConfigReload` 应用服务器请求**不会**刷新 MCP 连接，因此无头运行时在 MCP 配置变更后仍然需要重启。

## 远程网络权限

直接发往公网主机名的 HTTP/SSE 请求会校验每一条 DNS 应答，并把连接固定到公网地址。这同样适用于已配置的服务器、重定向以及 OAuth HTTP 请求。已配置的网络允许/拒绝策略不仅作用于 MCP 工具请求，也作用于登录和令牌刷新。

配置为 `localhost` 名称或私网 IP 字面量，即显式允许该本地端点。对于私有 DNS 名称，需要在服务器配置中主动选择启用：

```json
{
  "mcpServers": {
    "internal": {
      "url": "https://mcp.internal.example/mcp",
      "allow_private_network": true
    }
  }
}
```

`allow_private_network` 默认为 false。该例外只适用于所配置的源站（协议、主机和端口）；它不会授权其他重定向源站或 OAuth 源站。模型在会话中自行添加的服务器不能使用该例外，即使其配置中包含该标志也一样。

由运维方配置的服务器继续遵循 `HTTP_PROXY`、`HTTPS_PROXY` 和 `NO_PROXY`。当为已配置的源站选用了代理服务器时，目标地址的 DNS 解析和私网过滤会交给这个由运维方选定的代理服务器处理；本地的 DNS 固定无法约束代理服务器自身的解析。命中 `NO_PROXY` 时，则改用受保护的直连。模型添加的服务器、已审查的插件远程端点，以及次级的重定向/OAuth 源站，都不会继承环境中的代理权限。

## 远程 HTTP 认证

基于 URL 的 MCP 服务器可以使用静态请求头、环境变量派生的请求头、bearer-token 环境变量或 OAuth。授权优先级是保守的：

1. `headers` 和 `env_headers` 首先应用。
2. 若 `Authorization` 头尚未设置，`bearer_token_env_var` 会添加 `Authorization: Bearer <env value>`。
3. 仅当不存在 `Authorization` 头时，才使用存储的 OAuth 凭据。

对于 bearer-token 认证，推荐使用环境变量支持的配置：

```json
{
  "servers": {
    "remote": {
      "url": "https://example.com/mcp",
      "bearer_token_env_var": "EXAMPLE_MCP_TOKEN"
    }
  }
}
```

对于通用的远程 MCP OAuth，添加 URL 服务器并运行登录：

```bash
codewhale mcp add remote --url "https://example.com/mcp"
codewhale mcp login remote
```

Codewhale 会发现服务器的 OAuth 元数据，在你的浏览器中打开授权 URL，监听本地回调，交换授权码，并通过 Codewhale 的机密后端存储令牌响应。存储的 OAuth 令牌会按服务器名称加 URL 查找，并在可能的情况下于请求前刷新。登录期间，CLI 会打印授权 URL 和等待状态，同时本地回调监听器处于活动状态。如果基于 URL 的服务器在连接/发现期间返回 401 或 Unauthorized，`codewhale mcp connect <name>` 会报告需要 OAuth 认证，并指向 `codewhale mcp login <name>`。资源辅助工具的列表还会为认证类失败显示 `authentication_required` 条目，而不是静默地看起来为空。

可选的 OAuth 字段：

```json
{
  "servers": {
    "remote": {
      "url": "https://example.com/mcp",
      "scopes": ["tools/read"],
      "oauth": {
        "client_id": "public-client-id"
      },
      "oauth_resource": "https://example.com"
    }
  }
}
```

当提供商（provider）要求固定重定向时，用户级配置可以设置回调行为：

```toml
mcp_oauth_callback_port = 1455
mcp_oauth_callback_url = "http://127.0.0.1:1455/callback"
```

这些回调字段会在项目作用域的配置覆盖中被忽略。

## Hugging Face MCP

Hugging Face 为 Hub 资源、文档、数据集、Spaces 和社区工具提供了一个托管的 MCP 服务器。Codewhale 不会从 `/hf` 调用 Hugging Face 的 Hub HTTP API；它只帮助你检查并设置常规 MCP 管理器将要加载的 MCP 配置。

推荐的设置路径是 Hugging Face 的设置页生成的配置：

1. 登录后访问 <https://huggingface.co/settings/mcp>。
2. 选择最接近你的 Codewhale 配置形态的 MCP 客户端，并复制生成的服务器片段。
3. 将 Hugging Face 服务器条目粘贴到解析出的 MCP 配置文件中。
4. 运行 `/mcp reload` 重建模型可见的实时工具池。

Codewhale 同时读取 `servers` 和 `mcpServers`，因此设置页生成的片段可以在不更改 MCP 文件其余部分的情况下适配。纯占位符的形态如下所示：

```json
{
  "servers": {
    "huggingface": {
      "url": "https://huggingface.co/mcp",
      "headers": {
        "Authorization": "Bearer ${HF_TOKEN}"
      }
    }
  }
}
```

上面的占位符不是可运行的机密。请在私有 MCP 配置中使用设置页生成的值，并且永远不要提交真实的 Hugging Face 令牌。

交互式辅助命令：

```text
/hf mcp status
/hf mcp setup
/hf concepts
```

`/hf mcp status` 会检查已配置的 MCP 文件，查找常见的 Hugging Face 服务器名称或 Hugging Face MCP URL。`/hf concepts` 解释 Hugging Face 提供商路由、Hugging Face MCP 和显式 Hub 工作流之间的区别。

官方文档：<https://huggingface.co/docs/hub/hf-mcp-server>

## 配置文件位置

默认路径：

- `~/.codewhale/mcp.json`（当 Codewhale 文件不存在时，仍会读取 `~/.deepseek/mcp.json`）

覆盖方式：

- 配置项：`mcp_config_path = "/path/to/mcp.json"`
- 环境变量：`DEEPSEEK_MCP_CONFIG=/path/to/mcp.json`

`codewhale mcp init`（以及 `codewhale setup --mcp`）会写入这个解析出的路径。

交互式 `/config` 编辑器也会暴露 `mcp_config_path`。在 TUI 中更改它，会更新 `/mcp` 使用的路径，并将连接池标记为需要重载；随后 `/mcp reload` 会把实时连接池切换到新的配置来源。

编辑 MCP 文件或更改 `mcp_config_path` 后，运行 `/mcp reload`。无需重启 TUI。

## 工具命名

发现的 MCP 工具会以如下形式暴露给模型：

- `mcp_<server>_<tool>`

例如：名为 `git` 的服务器上的名为 `status` 的工具会变成 `mcp_git_status`。

命令面板包含按服务器分组的 MCP 条目。它会显示已禁用和失败的服务器，而不是隐藏它们，并使用与展示给模型的相同的运行时工具名。

## 资源与提示辅助工具

启用 MCP 时，CLI 还会暴露辅助工具：

- `list_mcp_resources`（可选 `server` 过滤器）
- `list_mcp_resource_templates`（可选 `server` 过滤器）
- `mcp_read_resource` / `read_mcp_resource`（别名）
- `mcp_get_prompt`

## 最小示例

```json
{
  "timeouts": {
    "connect_timeout": 30,
    "execute_timeout": 1800,
    "read_timeout": 120
  },
  "servers": {
    "example": {
      "command": "node",
      "args": ["./path/to/your-mcp-server.js"],
      "env": {},
      "disabled": false
    }
  }
}
```

为了与其他客户端兼容，你也可以使用 `mcpServers` 而不是 `servers`。

## 将 Codewhale 作为 MCP 服务器运行

你可以把本地 Codewhale 二进制注册为 MCP 服务器，这样其他 Codewhale 会话（或任何 MCP 客户端）就能调用它的工具。

### 快速设置

```bash
codewhale mcp add-self
```

这会解析当前二进制路径，生成一个运行 `codewhale serve --mcp` 的配置条目，并将其写入你的 MCP 配置文件。默认服务器名称是 `codewhale`。

选项：

- `--name <NAME>` — 自定义服务器名称（默认：`codewhale`）
- `--workspace <PATH>` — 服务器的工作目录

### 手动配置

`~/.codewhale/mcp.json` 中等价的手动条目：

```json
{
  "servers": {
    "codewhale": {
      "command": "/path/to/codewhale",
      "args": ["serve", "--mcp"],
      "env": {}
    }
  }
}
```

统一的 `codewhale` 运行时直接支持 `serve --mcp`，同时也提供等价的 `codewhale mcp-server` stdio 入口。发行版安装器会把同一个运行时以 `codew` 暴露出来；`mcp add-self` 会自动解析出调用它的那条命令。

### 前提条件

- `command` 引用的二进制必须存在且可执行。
- MCP 服务器通过 stdio 作为子进程运行——不需要网络端口。
- 每个 MCP 客户端会话都会生成自己的服务器进程。

### 工具命名

来自 MCP 服务器的工具遵循标准命名约定：

- `mcp_<server>_<tool>`

例如，来自默认服务器（名为 `codewhale`）的 `shell` 工具会变成 `mcp_codewhale_shell`。

### MCP 服务器 vs HTTP/SSE API vs ACP

| | `codewhale serve --mcp` | `codewhale serve --http` | `codewhale serve --acp` |
|---|---|---|---|
| **协议** | MCP stdio | HTTP/SSE JSON-RPC | ACP stdio |
| **用例** | 面向 MCP 客户端的工具服务器 | 面向应用的运行时 API | 面向 Zed/自定义 ACP 客户端的编辑器智能体 |
| **配置** | `~/.codewhale/mcp.json` 条目 | 直接 URL 连接 | 编辑器的 `agent_servers` 自定义命令 |
| **生命周期** | 按客户端会话生成 | 长时间运行的守护进程 | 按编辑器智能体会话生成 |

当你希望 Codewhale 工具对其他 MCP 客户端可用时，使用 `mcp add-self`。在构建直接消费 API 的应用时，使用 `serve --http`。当编辑器想以 ACP 智能体身份与 Codewhale 对话时，使用 `serve --acp`。

### 验证

添加之后，测试连接：

```bash
codewhale mcp validate
codewhale mcp tools codewhale
```

## 连接生命周期

会话启动时会在后台连接：每个已启用且被允许的用户配置服务器都会并发启动，不会阻塞第一个回合。把较重的服务器标记为 `lazy = true`，则在某个回合的 `allowed_tools`/`tools.always_load` 选择覆盖了它的 `mcp_<server>_*` 名称、某次模型调用解析到它的某个工具，或显式 `/mcp retry <name>` 请求它之前不会启动。标记为 `required` 的服务器即使是 `lazy` 也会在启动时连接，以便其失败在第一个回合之前暴露出来。尚未连接的服务器显示为 `not connected`，绝不会显示 `connecting`；`connecting` 标签只描述实际正在进行中的握手。OAuth 登录完成后，只有该服务器会自动重新连接。插件提供的服务器按需连接，除非标记为 `required` 或被会话的工具选择点名。

面向 MCP 的 `tool_search` 也是明确的发现请求：可搜索已配置的服务器名称
（例如 `engram`）、完整的 `mcp_<server>_...` 名称，或使用
`{"query":"mcp_.*","match":"regex"}`。当前回合的允许与拒绝范围先过滤服务器，
每批最多连接八台，沿用现有的五秒等待上限和取消机制。普通搜索不会启动所有
可选服务器。只有实际 `tools/list` 返回的模式才进入延迟工具目录；失败、禁用
或失去授权的服务器不会获得虚构工具。匹配超过八台时，请按服务器名称缩小搜索。

`codewhale mcp connect`、`validate` 和 `tools` 使用各自进程的连接池，不会向
正在运行的 TUI 或 exec 会话附加连接。请使用会话内的发现、明确工具选择，
或 `/mcp` 连接/重试。

## 服务器字段

每个服务器的设置：

- `command`（字符串，必需）
- `args`（字符串数组，可选）
- `env`（对象，可选）
- `connect_timeout`、`execute_timeout`、`read_timeout`（秒，可选）。服务器级的值会覆盖全局 `timeouts` 块，三个预算彼此独立：
  - `connect_timeout`（默认 30）覆盖进程启动、`initialize` 和首次 `tools/list`，因此首次通过 `uvx`/`npx` 下载包的时间也计入其中。
  - `execute_timeout`（默认 1800）是每次 `tools/call` 和 `prompts/get` 的预算。显式设置的更短值会被遵守，`read_timeout` 不会提前截断正在运行的工具。
  - `read_timeout`（默认 120）限制握手和工具发现期间每次等待回复的时间，也是 `resources/read` 的预算。
  - 请求超出预算时以超时错误失败。连接会被保留，之后迟到的回复会被丢弃，不会被交给其他调用。中断当前轮次会立即停止等待，但 Codewhale 目前还不会发送 `notifications/cancelled`，因此一次只处理一个请求的服务器会先完成被放弃的调用，再回答下一个请求。
  - Streamable HTTP 服务器在 POST 响应中直接返回回复；该 POST 同样受请求自身时限约束。若请求在发送阶段超时，连接会被关闭，并在下一次调用前重建。
- `disabled`（布尔值，可选）
- `enabled`（布尔值，可选，默认 `true`）
- `required`（布尔值，可选）：如果该服务器无法初始化，启动/连接验证会失败。
- `lazy`（布尔值，可选，默认 `false`）：在某个回合、工具调用或 `/mcp retry` 请求它之前不启动该服务器。不设置时，用户配置的服务器在会话启动时连接；插件提供的服务器按需连接，除非标记为 `required` 或被会话的工具选择点名。
- `enabled_tools`（数组，可选）：该服务器的工具名称允许列表。
- `disabled_tools`（数组，可选）：在 `enabled_tools` 之后应用的拒绝列表。
- `url`（字符串，可选）：远程 MCP 服务器的 Streamable HTTP 端点。
- `allow_private_network`（布尔值，默认 false）：运维方为该已配置源站上的私有 DNS 地址主动选择启用的开关；对模型添加的服务器无效。
- `transport`（字符串，可选）：对于传统 SSE 端点，设为 `"sse"`。
- `headers`（对象，可选）：基于 URL 的服务器使用的字面量 HTTP 请求头。
- `env_headers` 或 `env_http_headers`（对象，可选）：请求头名称到环境变量名称的映射。
- `bearer_token_env_var`（字符串，可选）：包含 bearer token 的环境变量。
- `scopes`（数组，可选）：`mcp login` 的默认 OAuth 作用域。
- `oauth.client_id`（字符串，可选）：预先注册的 OAuth 客户端 ID。
- `oauth_resource`（字符串，可选）：附加到授权 URL 的资源参数。

## 安全说明

MCP 工具与内置工具走相同的审批框架。只读的 MCP 辅助工具（资源/提示的列出与读取）在策略允许时，可以在 Ask 和 Auto-Review 中无提示地运行，而会产生副作用的 MCP 工具则需要审批。Full Access 不会绕过强制性的策略拦截。

你仍然应该只配置你信任的 MCP 服务器，并把 MCP 服务器配置视为与在你的机器上运行代码等价。避免提交字面量的 `Authorization` 请求头。优先使用 `env_headers`、`bearer_token_env_var` 或 OAuth 登录，让机密保持在 MCP 文件之外。

## 故障排查

- 运行 `codewhale doctor` 确认它解析出的 MCP 配置路径以及该路径是否存在。
- 在 TUI 中运行 `/mcp validate` 刷新可见的服务器/工具快照。
- 如果配置或凭据变更后模型的目录中缺少工具，运行 `/mcp reload` —— `/mcp validate` 只刷新 UI 快照。
- 如果 MCP 配置缺失，运行 `codewhale mcp init --force` 重新生成它。
- 如果工具没有出现，请验证服务器命令能否在你的 shell 中工作，以及服务器是否支持 MCP 的 `tools/list`。
