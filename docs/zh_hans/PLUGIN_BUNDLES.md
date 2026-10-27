# 插件包

> 英文原文：[PLUGIN_BUNDLES.md](../PLUGIN_BUNDLES.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。

Codewhale 的插件包边界刻意划得很小。这条边界在 v0.9.1 划下，v0.9.10 又审慎扩展：
插件包可以借助 Codewhale 已有的引擎，贡献声明式的 Skills、MCP 配置、Commands、
Agent profiles 和 Hooks。遇到不支持的声明，它只把这些声明记入清单，不会因此禁用
整个插件包——混合插件包依然可用。发现（discovery）本身从不执行、启用、信任、
下载、更新或安装任何东西。

本文负责插件包的格式、发现、校验，以及信任/启用/运行时契约。
[PLUGINS.md](./PLUGINS.md) 负责文件怎么写入磁盘、怎么删除，也就是 v0.9.4（#5182）
加入的 `/plugin install`、`update`、`uninstall`、`suggest` 上手入口。
兼容的 Claude Code 插件包使用同一套原生适配器，其支持子集见
[CLAUDE_PLUGIN_COMPAT.md](../CLAUDE_PLUGIN_COMPAT.md)。
可运行的原生示例，以及 OpenCode/DSH 数据如何显式转换，见
[编写你的第一个插件](./PLUGIN_AUTHORING.md)。

## 发现与优先级

Codewhale 只扫描自己的目录，在每个 `<name>/` 目录里查找以下清单之一：
`plugin.json`（原生 Agent Plugins v1.0.0 格式，自 v0.9.4 起）、
`kimi.plugin.json`（兼容的 Kimi Skills/MCP 子集，自 v0.9.8 起）、
`plugin.toml`（旧版 Codewhale 格式，仍可完整读取）、
`.claude-plugin/plugin.json`（兼容的 Claude 子集，自 v0.9.13 起）：

- 用户级：`~/.codewhale/plugins/<name>/`
- 工作区级：`<workspace>/.codewhale/plugins/<name>/`

一个插件包若发布了多种格式，依次读 `plugin.json`、`kimi.plugin.json`、
旧版 `plugin.toml`，最后是 `.claude-plugin/plugin.json`。
Computer Use 通过官方插件目录安装为普通用户级插件，独立于 Engine 更新；
安装后须审查并启用。Mac 原生助手单独安装，负责系统权限设置和电脑控制指示器。
详见 [Computer Use 指南](https://github.com/codewhale-hq/codewhale-cu-plugin)。

用户级插件优先于工作区级插件。符号链接形式的根目录、清单、组件路径和
嵌套组件文件一律失败关闭。历史内置快照和回执保留，供旧二进制使用；
当前 Engine 不再创建或发现内置 Computer Use 副本。
从旧布局升级时，须从插件目录安装并审查一次。

新建的用户级和工作区级插件包一律未受信任且处于停用状态。发现过程是只读的，
不会检查其他应用的扩展目录或凭据目录：`.claude/plugins`、`.cursor/plugins`
这类环境根目录从不扫描。

v0.9.1 之前的 `overrides.json` 启用状态，刻意不作为信任导入；每个插件包都只能通过
下面这套审查来激活，该审查依据内容哈希和 `codewhale-plugin-capabilities-v3` 激活策略。

## 清单

所有受支持的编码格式都会解析成同一份内部清单，因此下游的校验、哈希、审查和运行时行为完全一致。
这些格式之间刻意不做磁盘上的自动迁移；`/plugin export <name> <target-dir>`
会把已加载的插件包按 Agent Plugins v1.0.0 规范导出到一个目录，不改动已安装的那份。

### `plugin.json`（Agent Plugins v1.0.0）

这份标准的清单根是封闭的：只允许 `$schema`、`name` 和几个可选的标准字段
（`version`、`description`、`author`、`homepage`、`repository`、`license`、
`keywords`、`extensions`）。出现未知的根键就是解析错误。客户端专用数据放在
`extensions` 下，按反向域名命名空间归类。未知的厂商命名空间一律忽略，从不拒绝——
正因如此，为其他客户端编写的插件包才能在这里加载；而 Codewhale 自己的
`extensions["net.codewhale"]` 命名空间里若出现未知键，则直接拒绝，不会静默丢弃。

名称遵循标准规则：1–64 个小写 ASCII 字母、数字，或位于中间的单 `-`/`.`，
首尾必须是字母或数字，不允许 `--` 或 `..`。插件包根目录下的 `skills/` 目录会自动识别；
其他组件位置、`capabilities`、`when` 和 `display_name` 都放在
`extensions["net.codewhale"]` 里。

MCP 服务器不能写在 `plugin.json` 里（根是封闭的）；它们放在同级的 `mcp.json` 中，
位于 `mcpServers` 下，传输方式为 `stdio`、`streamable-http` 或 `sse`
（`type` 可以省略，省略时根据 `command` 还是 `url` 推断）。
Codewhale 独有的服务器选项——超时、工具过滤、基于环境变量的凭据、启用状态——
按服务器写在 `extensions["net.codewhale"]` 下。`env` 名称 `PLUGIN_ROOT` 和
`PLUGIN_DATA` 由标准保留给宿主运行时，在插件定义里会被拒绝。

### `plugin.toml`（旧版 Codewhale 格式）

```toml
schema_version = 1

[plugin]
name = "example"
version = "0.1.0"
description = "Example instruction and MCP bundle"
author = "Example Author"

[skills]
path = "skills"

[commands]
path = "commands"

[agents]
path = "agents"

[hooks]
path = "hooks"

[mcp_servers.local]
command = "node"
args = ["server.js"]
cwd = "mcp"

[mcp_servers.remote]
url = "https://example.invalid/mcp"

[capabilities]
network_hosts = ["example.invalid"]

[when]
os = ["macos", "linux", "windows"]
binaries = ["node"]
```

旧版 TOML 的名称由 1–64 个小写 ASCII 字母、数字或位于中间的连字符组成。
没有 `schema_version` 的未加版本号清单仍可解析，但 `/plugin validate` 会给出迁移警告
（`[plugin].version` 缺失时显示为 `0.0.0`）。出现未知的顶层表或字段就是解析错误
（`deny_unknown_fields`），报告时给出字节偏移，但不回显清单里的值。

### 校验（两种格式）

组件路径必须是相对路径、位于插件包内、真实存在，而且不能包含符号链接或
Windows reparse point（包括 junction 和 mount point）。v1 schema 会拒绝未知的
MCP 字段、含义有歧义的本地/远程传输组合、没有上限的列表或超时，
以及相互重叠的工具过滤。

远程 MCP URL 必须使用 HTTPS，只有显式的回环 HTTP 端点例外。URL 中不能含用户信息、
查询串或片段。字面量请求头一律拒绝：身份验证必须通过 `env_headers` 或
`bearer_token_env_var` 指定一个来源环境变量。远程插件包必须在
`capabilities.network_hosts` 中精确声明其端点用到的归一化主机集合；
端点的协议、归一化主机、端口和路径都绑定到本次审查。重定向有限制，
且必须保持完全相同的归一化 origin。经过审查的远程传输会使用一个显式禁用代理的 HTTP 客户端：
插件包从不读取或使用环境中的 `HTTP_PROXY`、`HTTPS_PROXY` 或 `NO_PROXY`，
因为代理凭据和代理可见的内容，都不在受审查的权限范围内。
用户自己写的 MCP 配置保留现有的显式代理支持。

本地 stdio 环境项必须使用精确的 `${SOURCE_ENV}` 引用。审查界面会显示目标名和来源名，
但从不读取或打印它们的值。插件子进程只继承 Codewhale 清理过密钥的基础环境，
外加这些经过审查的映射；带凭据的代理变量，以及用户自己写的 MCP 配置所用的更宽泛的兼容环境，
都不会从环境中继承。绝对路径参数和向上穿越父目录的路径会被拒绝；
位于插件包内的入口点会在启动前冻结到暂存路径。

审查时，每个 stdio 参数都会以 JSON 字符串无损展示。常见的携带凭据的参数标志
和已知的字面量 token 形态都不允许出现在 argv 中；凭据必须改用经过审查的环境变量映射。
插件贡献的 MCP OAuth 自 v0.9.1 起已禁用，到 v0.9.6 仍然禁用，
发现、登录、刷新和 token 存储都包含在内。清单若在插件的某个 MCP 服务器上声明 OAuth
字段，校验就会失败。

### 生效与未生效的组件面

Codewhale 0.9.10 会从内容寻址的运行时快照中激活声明式的 `[skills]`、
`[mcp_servers.*]`、`[commands]`、`[agents]` 和 `[hooks]` 组件。
Commands 使用 Markdown 命令文件，Agents 使用 Fleet TOML profile，
Hooks 使用 `HooksConfig` TOML 文件。一个组件可以指向单个文件，
也可以指向存放同类文件的目录。普通的用户级/工作区级命令和 Agent profile
优先于插件贡献；受信任的项目 hook 在插件 hook 之后运行。

清单还可以额外登记下面这些未生效的组件面。这些声明照旧参与哈希、审查和展示，
但不会激活，也不再因此禁用整个插件包：

```toml
[lsp]           # TOML alias: [lsp_servers]
path = "lsp"

[native]        # TOML alias: [native_extension]
path = "native/index.mjs"

[capabilities]
filesystem_roots = ["workspace"]
network_hosts = ["api.example.invalid"]
lifecycle_mutation = true
```

（在 `plugin.json` 插件包里，同样的表放在
`extensions["net.codewhale"]` 下。）

接受也好，拒绝也好，都刻意做得显眼，绝不静默：

- 兼容性按组件判定：所有声明的组件面都有适配器时是 `full`（空插件包也算）；
  受支持组件能与具名的未生效组件面并存、照常激活时是 `partial`；插件包只声明了
  Codewhale 暂时还无法激活的组件面时是 `unsupported`。同一套带版本号的激活策略（v3）
  驱动这些标签、运行时适配器和能力哈希。将来某个 Codewhale 版本开始执行 LSP
  或原生代码时，必须修改该策略；策略一变，能力哈希随之改变，就会强制重新审查。
  v1 和 v2 信任回执会以 `capabilities-changed` 失败关闭。
- **实验性扩展宿主下的 `native`。** 开启 `[features] extension_host` 后，策略升级为
  v4，`native` 成为已生效的适配器：每个条目是一个 `.mjs`、`.js` 或 `.mts` ES 模块文件，
  由 TypeScript 扩展宿主导入。指向目录或其他任何文件时，`/plugin validate` 和审查界面
  都会报错，并阻止激活。它的工具始终使用 `Required` 审批；Full Access（完全访问）、
  Bypass，或针对该已审查构建的精确会话授权，都可以满足这一要求而不再弹出审批。
  切换该开关会使每个插件都需要重新审查。详见
  [设计文档](../design/TS_EXTENSION_HOST.md#as-built-phase-1-2026-09-25)。
  [扩展作者指南](../EXTENSIONS.md)包含经过测试的类型化示例、诊断流程，
  以及 Node 可擦除 TypeScript 语法的限制。
- **已识别但未生效**的声明（`lsp`、`native`、非空的
  `capabilities.filesystem_roots`，或 `capabilities.lifecycle_mutation = true`）
  会像其他组件一样解析、校验（位于包内、存在、无链接）。它会计入清单，
  参与能力回执的哈希计算，在审查界面和 `/plugin show` 里显示为未生效，并且永不执行
  （对 `native` 而言，仅限于扩展宿主开关处于关闭状态时）。
  经过审查、受信任且适用的混合插件包仍可启用：受支持的声明式组件照常生效，
  未生效的组件面仍旧标为未生效。
- **全部不受支持**的插件包可以审查、可以信任，但 `/plugin enable` 会失败关闭，
  并列出未生效的组件面。没有任何组件能被诚实地激活。
- **无法识别**的段或字段属于校验失败，而不是清点条目：未知的顶层 TOML 表、
  未知的 MCP 服务器字段、未知的 `plugin.json` 根键，以及
  `extensions["net.codewhale"]` 里的未知键，统统直接拒绝。
  唯一“忽略而不报错”的情况是 Agent Plugins 格式中其他厂商的 `extensions`
  命名空间，标准要求客户端跳过它。
- `capabilities.network_hosts` 不属于未来才会生效的组件面：它今天就已强制执行，
  且必须与插件包远程 MCP 端点的归一化主机集合完全一致
  （有远程 MCP 端点就必须声明它，没有远程端点就不能声明）。

环境检查或健康检查通过，永远不等于信任。

## 审查、信任与启用

使用会话内的命令界面：

```text
/plugin list
/plugin validate example
/plugin show example
/plugin enable example
```

第一次 `enable` 会打开审查界面，显示来源、组件清单、申请的权限、脱敏后的 MCP 端点、
完整的内容哈希和能力哈希，以及未生效的声明。它还会打印一条确认命令（需逐字执行）：

```text
/plugin trust example <full-content-sha256>.<full-capability-sha256>
```

审查完插件包之后，再执行这条完整的命令。确认 token 用的是两个完整的 SHA-256 回执，
不是显示用的前缀。能力回执是 v3 摘要：它照旧哈希完整清单，
同时还绑定本次构建的激活策略（哪些适配器可执行、哪些只清点）。建立信任时，会先把审查过的
整棵树复制到 Codewhale 自有、内容寻址的运行时快照里，并记录匹配的回执；它不激活任何东西。
然后再次运行 `/plugin enable example`。信任与启用是两件事：

- `/plugin disable example` 停止贡献，但保留信任。
- `/plugin revoke example` 撤销信任，但保留启用位；插件包在重新审查之前一直不生效。
- `/plugin reload` 在磁盘文件发生变化后重建当前工作区的注册表。

（`/plugin install`、`update` 和 `uninstall` 负责把文件本身放好、替换和删除，
最后总会落到同一套审查上——见 [PLUGINS.md](./PLUGINS.md)。
`/plugin suggest` 会对已安装的插件包，以及本地添加的市场目录排序；发送匹配的任务时，
可以用 toast 提示弹出同样的下一步，但不安装任何东西。模型的请求里不会写入任何插件推广内容；
完整的推荐策略见 [PLUGINS.md](./PLUGINS.md#codewhale-如何推荐插件)。）

信任、启用、停用、撤销和重新加载都会立即重建当前工作区的 Skills、MCP、Commands、
Agent profiles 和 Hooks。每次持久化的状态变更，都会在稳定的跨进程锁下，
推进各个插件包自己的代数（generation）。代数一变，就会取消正在进行的 MCP 工作，
移除缓存中的目录条目，终止空闲的插件 stdio 子进程，
并拒绝那些已持久化入队、携带旧权限回执的 Skills。

审查界面区分远程 MCP 端点和本地 stdio MCP 服务器。本地 stdio 服务器是一个子进程，
以 Codewhale 用户的宿主文件系统和网络权限运行；插件信任不是操作系统沙箱。
因此审查界面会显示命令、参数个数、工作目录、环境变量名，以及这条宿主权限警告，
但不打印环境变量值或请求头值。服务器启动后，MCP 工具审批仍然照常生效。

信任回执存放在 `~/.codewhale/plugins/state.json`。写入是原子的、仅属主可读写，
记录完整内容哈希、能力哈希、已审查的能力清单、代数和审查时间，
并保留最近 32 次审查作为有界的审计记录。状态文件格式错误或不受支持时不会被覆盖：
在修复或移走该文件之前，所有插件包都失败关闭。

内容哈希覆盖清单、整棵插件包树和可执行形态，按确定的路径顺序计算，
包括本地 MCP 入口点和随附资源。暂存有大小上限：它会拒绝符号链接和不受支持的文件类型
（外加所有 Windows reparse point 和硬链接文件），用原子方式替换目标目录；
在 Windows 上，它通过经过校验的对象句柄施加仅属主可用的运行时权限或 ACL。
能力哈希覆盖归一化后的组件与权限清单。源文件或暂存内容被改动、能力发生变化，
或者运行时的根被不安全地替换，都会确定性地使回执失效；已经启用的插件包会变得不生效，
直到重新审查。`/plugin update` 依赖的正是这套失效机制：字节被替换后与回执不再匹配，
就会强制重新审查。

## 运行时行为

生效的插件包必须已启用、当前哈希已受信任、适用于本机，并且没有校验错误。
经过审查的混合插件包可以生效，但只有审查过的 v3 激活掩码里受支持的组件可以使用。
不受支持的组件仍然列在清单里、参与哈希和审查，但不生效。

- Skills 只以 `<plugin>:<skill>` 的形式暴露。面向模型的目录和 `load_skill`
  使用一份绑定到已审查暂存树的内存快照，而不是在执行时去读可变的源路径。
  `load_skill` 在放出内容之前会立即重新校验来源、暂存、回执、工作区和代数。
  一旦发现漂移，就失败关闭。排队的消息会持久化同样的来源信息，
  并在派发时重复这次检查。`/skills inspect` 会指出它来自哪个已审查的插件包，
  但不暴露其可变的源路径。
- MCP 服务器名暴露为
  `plugin-<plugin-name-byte-length>-<plugin>-<server>`，
  这样任一部分里的连字符都不会造成权限冲突。停用或未受信任的插件包
  会在无头 MCP 适配器处再次被拒绝。权限检查发生在这些时机：连接之前、
  每次惰性 stdio 启动的前一刻、传输层构建之后，以及每个工具/资源/提示操作之前。
  操作进行期间还会监视持久化的代数/启用/信任状态，因此停用、撤销
  或其他跨进程状态变更会取消该操作，并终止插件的 stdio 子进程。
  完整的源树和暂存树哈希会在派发和目录边界处重新校验；
  对已经跑起来的 MCP 调用，运行时不会持续重新哈希这些树。所以源或暂存树的漂移
  要到下一个边界才会失败，届时丢弃过期的连接或目录条目，
  但不宣称能中断正在执行的调用。每次失败都会附带提示：重新加载、审查、信任，
  然后再次启用插件包。
- Commands 在普通用户级/工作区级命令和已保存工作流之后加载，
  因此已有定义保持优先，冲突也看得见。命令面板会立即隐藏已撤销的命令；
  派发时会先重新核对完整回执，再展开命令体；遇到过期输入，会给出可见的拒绝提示。
- Agent profiles 加入 Fleet 名册时，排在显式配置、个人 profile 和 workspace profile
  之后，但在内置项之前。名册冲突时，那条可见的遮蔽记录照旧保留。每次启动 Agent
  都会依据当前注册表重新构建，并在使用所选插件 profile 的提示词或路由之前，重新核对它的权限。
- Hooks 在全局 hook 之后、受信任的项目 hook 之前合并。前台 Hooks 会在启动进程之前
  立刻重新核对权限；后台 Hooks 在入队前检查，出队时再检查一次，
  这样已入队但被撤销的 Hook 也不会稍后启动。
- 普通启动、resume、fork、exec 和 serve 都会先构建一份不可变的工作区级注册表，
  再构建各自依赖插件的目录。
- 宪章（Constitution）、仓库指令、权限规则、沙箱策略和 MCP 工具审批的优先级，
  始终高于插件指令。

`/plugin list`、`show`、`suggest` 和 `validate` 不发起网络请求、不启动进程、
不读取凭据、不写配置。审查界面把结构化 argv 渲染为无损的 JSON 字符串，
并展示环境变量的来源，而不显示其值。携带凭据的 argv 在清单校验阶段就被拒绝；
由插件产生的错误会屏蔽 URL 查询、身份验证、argv 和环境变量内容。
`[tools].plugin_dir` 下的旧版可执行工具仍是独立的一套系统，列在 `/plugin tools` 下。

## 截至 v0.9.10 的明确非目标

联邦式市场目录（`/plugin marketplace add|list|show|remove|install`）
会解析本地 Kimi、Claude、Codex 和 Codewhale 格式的目录文档，见下面的市场小节
（`/plugin install` 只拉取一个经过审查的来源，`/plugin suggest` 只对已安装的插件包排序）。
此外，以下这些同样明确不做：环境兼容性发现、自动信任、插件贡献的 MCP OAuth、
LSP 适配器和 MCP 订阅适配器；除实验性的 `extension_host` 开关之外，没有原生扩展运行时；
不导入外部的可执行插件运行时；也不把旧版 `plugin.toml` 自动迁移为磁盘上的 `plugin.json`。显式的离线
[OpenCode/DSH 转换器](./PLUGIN_AUTHORING.md#转换现有插件)支持选定的可移植 Skills、
静态的 Streamable HTTP MCP 声明，以及用 `--stdio-root` 明确选中的已打包 Node
`.mjs`、`.js` 或 `.cjs` MCP 服务器。转换时会把本地源码和依赖一并复制，
让它们走完同样的原生安装、能力审查、哈希绑定信任和启用流程；转换本身不执行代码，
也不调用包管理器。它不会迁移任意插件包，也不会复现其他客户端的运行时或策略。
上面提到的其他能力属于后续工作，而不是已隐含支持。

## 市场目录（#5311）

`/plugin marketplace` 按真实发布的 schema 读取本地目录文档
（Kimi、Claude、Codex、Codewhale 原生；Codex 通过其策略标记），
并为每个候选项渲染一份诚实的安装计划：

```text
/plugin marketplace add <name> <path>   # parse a local catalog file (no network)
/plugin marketplace list                # catalogs + candidates + diagnostics
/plugin marketplace show <name>          # one catalog in detail
/plugin marketplace remove <name>        # forget a catalog (plugins unaffected)
/plugin marketplace install <catalog> <candidate>
```

- `add` 从不拉取任何东西：它读取一个本地 JSON 文件（≤4 MiB，只接受普通文件，
  拒绝符号链接），并把解析结果存放在插件状态文件旁边。
- 目录层级和来源标记（`official`、`curated` 等）**仅用于显示**——
  它们从不授予信任、启用或安装。
- 外部策略会被显式忽略：Codex 的 `INSTALLED_BY_DEFAULT` 条目会列出来，
  但带 `NO_AUTO_INSTALL` 警告，在操作者运行安装命令之前不会安装任何东西。
- Codewhale 无法拉取的来源（npm 包、`command:` 来源、非 tarball URL）
  会标注为 `not installable` 并给出原因。
- `install` 走与 `/plugin install` 相同的审查安装流程：插件包落盘时是停用且未受信任的，
  在激活之前要先进入哈希绑定的信任审查。
