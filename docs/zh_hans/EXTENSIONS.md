# 编写扩展工具、命令与 Native mod

> 英文契约：[EXTENSIONS.md](../EXTENSIONS.md)。本指南描述当前已实现的宿主接口。

实验性的 TypeScript 扩展宿主在共享 Node 进程中运行已审查的插件代码，Bun 可显式
选择。Rust Engine 仍负责会话、工具准入、审批、权限和执行。插件可以贡献工具、用户
命令、执行前提议、提示词片段及插件本地 JSON 状态；不能提供审批服务、替换核心
工具或命令，也不能运行另一套 agent loop。

## 从可运行示例开始

[hello-extension](../examples/plugins/hello-extension/hello.mts) 演示工具和命令。
[mod-extension](../examples/plugins/mod-extension/README.md) 进一步演示 scoped hook、
提示词片段、保存计数、结构化结果和清理。后者是普通 ESM，不需要编译器或安装依赖。

1. 在配置中显式设置 `[features] extension_host = true`，或使用
   `codewhale --enable extension_host` 启动。功能开关放在子命令前。
2. 在 Codewhale 会话中运行 `/plugin install ./docs/examples/plugins/mod-extension`。
3. 运行 `/plugin validate mod-extension` 和 `/plugin show mod-extension`，审查 Native
   能力、源代码和权限。
4. `/plugin enable mod-extension` 会在缺少信任时显示审查流程。由本人执行审查给出的
   精确信任命令，然后再次启用。安装后默认未启用、未信任；信任本身不执行代码。
5. 明确要求模型调用 `mod_counter`，参数为 `{"label":"first"}`。工具走普通审批流程。
   `{"increment":false}` 只读计数，`{"label":"blocked"}` 演示监听器拒绝。
6. 本人输入 `/mod-count` 可显示保存的计数；`/plugin disable mod-extension` 会移除工具、
   命令、监听器和提示词片段，保留计数。

协助编写插件的 agent 应在安装和验证后把审查交给本人，不应自动信任或启用插件。

## 入口、审查与生命周期

新插件包使用 Agent Plugins v1.0.0 的 `plugin.json`。在
`extensions["net.codewhale"].native.path` 指定包内的普通 `.mjs`、`.js` 或 `.mts`
ES 模块文件；功能开启时目录及其他后缀会被拒绝。功能关闭时，Native 入口只列入
清单，不执行。用户目录是 `~/.codewhale/plugins/`，工作区目录是
`<workspace>/.codewhale/plugins/`。

`native.paths` 可声明多个入口，最多 64 个。它们按声明顺序激活，共用一个所有者；
某个入口激活失败时，只撤销该入口：已激活的入口保持注册，后续入口仍会激活；只有
没有任何入口激活成功时，整个插件才会失败。相同文件重复列出只算一个入口。源文件、权限或已审查内容变更需要重新审查。`/plugin reload` 是显式刷新，
不提供自动监视或热重载。停用、撤销信任、所有者故障及宿主退出会撤销对应注册。

注册返回可重复调用的 disposer，并归属于调用它的 Cordis fiber。可用 `ctx.effect`
登记其他清理工作；异步清理会等待，但有两秒上限。不要在清理时继续使用 storage：
所有者开始清理后该接口即不可用。详见示例的队列结束与注册撤销代码。

## 运行时与依赖

Node 是默认运行时，要求 `^22.19 || >=24`。Bun 要求至少 1.4.0，目前只在 macOS
完成资格验证，默认切换需要另行记录。用户可通过 `[extension_host] runtime`
选择 `node`、`bun` 或 `auto`；显式配置的运行时路径失败时，不会默默搜索替代路径。
`auto` 的回退原因会显示在 `/plugin` 和 doctor 诊断中。

默认的 `mcp_backend = "host"` 使用固定版本的 MCP SDK，不必启用 Native 扩展。
`[features] extension_host` 只控制可选 Native 扩展。已有 stdio 和 HTTP 服务器在支持时
使用现代发现协议；显式配置的旧版 SSE 服务器仍使用旧版握手。进程与网络权限、凭据解析、
目录接纳、工具批准和取消仍由 Rust 掌握。SDK 只收到不含真实地址或凭据的会话标识和
精确的逐次操作授权。现代请求携带 Rust 批准的协议元数据和请求头，不使用会话 ID。

只有明确、类型化的发现拒绝才能允许旧版握手；只有握手期间明确的 HTTP 传输拒绝才能
通过新的精确 Rust 授权切换到旧版 SSE。旧版 HTTP 保留 GET 预检和有界 OAuth 刷新；
现代 HTTP 不继承这些会话和重发行为。不完整的结果流程会报告错误，不会自动重放操作。
凭据、配置中的 URL、OAuth 浏览器登录和 Computer Use 决策密钥始终保留在 Rust。

两个后端共享 MCP 语义基准，并分别保留包含发现、认证和会话证据的传输基准。
Host 默认切换须通过这些基准、实际代理验收，以及 Ops CURRENT_DECISIONS §26 的平台
隔离与实测门槛。资格验证完成后的一个版本内，仍可显式设置 `mcp_backend = "rust"`，
之后删除原生协议适配器。缺少受支持的 Node 时沿用 doctor 的运行时诊断；Host 失败
不会自动选择 Rust。

`.mts` 只支持 Node 能直接擦除的类型语法。enum、decorator、JSX 等需要转换的语法，
应由作者先构建为 JavaScript。不要依赖 Bun 特有的 tsconfig 路径解析：Node 不读取
此配置。Bun 使用 `--no-install`，缺少依赖时会失败，不会下载。把运行时依赖和本地
导入打包在已审查包内；激活时不应安装包或获取代码。

已审查的 DSH 组合在导入前会核对本地模块的路径和 SHA-256。Node 检查运行时解析；
Bun 解析已审查的 JavaScript 语法，并在执行计算式导入前核对对应凭据。未审查的文件
和环境中的包会被拒绝。Bun 目前无法保留查询参数或片段所区分的模块身份，动态
CommonJS 解析也需要 Node；这些情况会明确提示使用
`[extension_host] runtime = "node"`。普通 UTF-8 源码和已审查的计算式文件/JSON
导入可在两种运行时中使用。

宿主提供同一个 Cordis、schemastery、cosmokit 及有限的 DSH 兼容导出。支持的服务名为
`tools`、`commands`、`prompt`、`storage`、`skills`、`shellHooks`、`avatars`、`mcp`、`logger`、`events`、`reflect`、`registry`。`ctx.avatars.registerPack({path})` 注册已审查包内的头像包，
参见 [whale-avatar 示例](../examples/plugins/whale-avatar/README.md)。
需要未提供服务的插件会激活失败，并显示原因。目前没有已发布的插件编写 SDK；
普通 ESM 示例使用这些文档规定的适配服务。

共享进程内的插件并不相互隔离；所有者标签及 `dataDir` 不能构成彼此之间的安全边界。
宿主禁止进程内原生代码入口，包括 native addon、Worker 和相关 FFI/SQLite 入口。
macOS Seatbelt 与 Linux bubblewrap 限制宿主进程，但不等于对每次插件文件访问执行
工具审批。不要在描述、日志、配置或结果中保存凭据。

Native 在 macOS 和 Linux 使用文件读取允许列表，只开放运行时及必需系统文件、宿主
包、Core 管理的已审查快照目录、Native 数据和私有临时目录。普通项目文件（包括
`.env`）不在授权范围内，子进程继承相同限制。共享 shell 配置中的 Cargo、npm 和
Darwin 缓存写权限不适用于 Native。快照目录及数据仍属于共享 Native 信任组，
并不提供插件之间的隔离；直接访问项目的 git 插件需要另行通过受限操作桥接。

Native 扩展启动必须通过操作系统隔离验证。Linux 缺少 bubblewrap 或命名空间探测失败
时会拒绝激活并显示具体原因；Windows 使用已有 LPAC 包装器，无法建立文件与网络
隔离时拒绝 Native。源码支持不等于已完成对应平台及发布产物验证。
固定摘要的 Builtin 宿主可使用明确标示的未隔离例外，但其所有效果仍须兑换 Rust
签发的操作凭据。这不能证明 Native 隔离或默认运行时切换已完成。

金融、数据与语音工具的实验性适配器分别使用 `[features] finance_host = true`、
`data_host = true` 和 `speech_host = true`；三者默认使用 Rust。CLI `speech`/`tts`
与模型工具共享语音准备逻辑，克隆样本、提供商请求和输出文件写入仍归 Rust。
选中 Host 后通过现有固定 Builtin harness 与
操作 broker 执行，即使 Native 扩展关闭也可独立运行。网络、文件、解析诊断、权限及凭据仍归
Rust；选中的 Host 失败会明确报告，不会自动切回 Rust 适配器。

## 工具和结构化结果

用 `ctx.tools.register({name, description, parameters, execute})` 注册工具。名称最多
64 个字符，以字母开头，只使用字母、数字、`_`、`-`。使用插件专属前缀；核心名称、
审批名称族及 `mcp_`/`ext_` 前缀保留。核心及其他插件的工具不能被覆盖。

Rust 在审批前以及发送给宿主前检查 JSON Schema，包括类型、必填字段、大小限制和
`additionalProperties: false`。外部 `$ref` 不会被获取，无法编译的 schema 会拒绝注册。
插件自行声明的只读属性不能取消审批：扩展工具始终是 `Required`。Full Access、Bypass
或针对该已审查构建的精确会话授权可以满足外层要求。注册本身不授权执行。

`execute(input, exec)` 返回文本或 JSON。普通 JSON 会保留为工具结果的结构化数据，
同时生成模型可读的文本输出。`mod_counter` 返回计数及标签。结构化结果不会执行 UI
代码；自定义工具卡片和 Ratatui/GPUI 控件尚未提供。

DSH 定义若声明 `output`，必须同时提供 `output.schema` 和 `output.render`。
宿主先验证返回的 JSON，再执行渲染。目前只支持普通文本块；富媒体及额外字段会明确
报错，不会被静默丢弃。非空注册选项、`projectContent`、`finalizeContent`、
`timeoutMs` 和 `when` 仍不支持。布尔值 `deferLoading` 仅作为展示提示接受，
不会改变 Engine 的工具加载策略；非布尔值会被拒绝。共享
[`compatibility.json`](../../crates/engine/extension-host/src/dsh/compatibility.json)
记录 DSH 0.2.1-rc.5 定义 API 与服务、模块映射，其内容参与 Native 内容和能力摘要；
契约改变后必须重新审查。成功及软失败结果中的 `extension` 元数据由 Rust 写入，
记录插件、已审查内容、入口、代际、调用方选择、句柄与进程启动身份。

工具调用有 120 秒截止时间，取消通过 `exec.signal` 传递。插件应检查并向自己发起的
异步工作传递 signal；忽略取消的 JavaScript 仍可能在共享进程中继续运行，直到宿主
被终止。不要在共享进程中使用 `process.chdir`。

## 用户命令与调用身份

`ctx.commands.register({name, description, argumentHint?, handler})` 注册用户输入的
斜杠命令。名称用小写字母、数字、`_`、`-`，以字母开头，最多 64 字符。命令不能覆盖
内置命令、别名或其他插件命令；同名 Markdown 用户/工作区/插件命令优先。

handler 只在用户调用时运行，可返回文本、`{kind:'success', text?}`、
`{kind:'error', text}` 或 `{kind:'submit', prompt, text?}`。`submit` 会明显地把 prompt
以 Runtime 来源和插件归属进入原有回合，后续模型工具仍经普通权限和审批流程。命令不能直接调用模型、
工具或审批，截止时间为 30 秒。输出去除终端转义序列并限制为 64 KiB，超过 128 KiB
的提交 prompt 会被拒绝。

工具上下文包含 `signal`、`callId`、`args`；命令包含 `signal`、`commandId`、
`args`、`rawInput` 和空的 `attachments`。两者都可能获得本次调用的 `workspace`、
插件自己的 `dataDir` 及只读 `sessionId`、`agentId`、`originTurnId` 字符串。
Rust 未提供的字段缺省；激活时不缓存会话身份。这些字符串只用于关联，不授予操作
会话或 agent 的权利。用户命令通常只有已知的 session 标签。
当 Rust 提供会话和工作区时，工具调用还包含冻结的 `exec.agent.id` 和
`exec.agent.session.header.cwd`；根身份使用会话 id，子 agent 使用 Core 提供的 id。
投影本身没有方法；保留该对象不会获得调用结束后的操作权限。

注入 `sessions` 的插件可在直接调用的根工具持有 Engine checkpoint ticket 时调用
`ctx.sessions.flush(exec.agent.session)`，参数必须是本次调用的原始 session 对象。
Engine 捕获已提交的前缀，由所属 Runtime、终端或 exec 存储实际写入并确认。
Runtime 保留现有的回执秘密脱敏规则；成功确认的是持久化投影，不包含当前工具
未来的结果或插件自己的日志。恢复时保留后续工具结果，不重新执行工具。
含 checkpoint 的 Runtime turn 使用 schema 5，旧读取器会拒绝，避免静默丢失历史。

只有明确选择无持久化后端的临时宿主返回 `false`。缺少权限、外来或过期对象、
子 agent checkpoint、被后续请求替代的写入、存储失败、取消及不可用的消费者均拒绝。
取消可能与磁盘写入竞争，因此拒绝不能证明磁盘完全未写入。其他 `sessions` 方法
仍未支持。

注入 `subagents` 的插件可在直接调用的根工具中启动一个独立子 agent。
`ctx.subagents.getProvider('spawn')` 描述唯一的提供者：不继承对话的全新子 agent。
`ctx.subagents.startActivation({ provider: 'spawn', childId, request: { parent:
exec.agent, prompt, agentOptions: { model } }, signal, delivery })` 先运行该插件自己的
`agent/created` 监听器：其中子 agent 的 `ctx.systemPrompt.section({ name:
PERSONA_PREFIX_SECTION, ... })` 设置最多 32 KiB 的角色设定，追加到该子 agent 自己的系统提示并记入其启动清单；
`ctx.tools.restrict({ deny })` 只会增加禁用工具。随后 Engine 通过本轮的审批流程以普通 `agent` 启动该子 agent：
插件的调用需要单独审批，不会复用模型的授权。子 agent 运行在 Codewhale 现有的子 agent 管理器中，
受父级权限和预算限制。返回的对象在子 agent 接收首条消息后才完成；`result` 等待子 agent 结束并返回文本输出与停止原因；
`dispose()` 停止该子 agent 及其后代。`delivery: 'caller'` 只把结果交给插件，不交给父模型。
插件未释放的子 agent 会在调用结束时停止。`ctx.tools.get(name, exec.agent)` 报告调用方是否拥有某个工具。
提示词仅支持文本，agent 选项仅接受 `model`；子 agent 自己的工具调用中不可使用该服务；非文本输出及其他 DSH agent API 尚未支持，
`agents` 与根级 `systemPrompt` 不提供方法。

当 Rust 提供调用者会话时，`invocation.agent` 另外提供只读 id 和
`followup(message)`。一次调用只接收一条 JSON user 消息，包含有界 id、仅有 kind
的 source 和一个纯文本块；成功返回时转为相同的 Submit 结果。错误、异常或取消
不会提交。多条消息、与显式 Submit 混用、附件、空文本及超过 128 KiB 的文本均被
拒绝，调用结束后句柄失效。Core 决定实际回合标识及来源；这不是通用 agent 服务，
也不持久化传入的 DSH 消息标识。缺少调用者上下文时拒绝访问。

原生 App 通过 `GET /v1/commands?thread_id=<id>` 获取当前会话的扩展命令及不透明
`registration_id`。调用 `POST /v1/threads/<id>/commands` 时发送 `name`、`args`、
`registration_id`、`operation_key` 和 `expected_workspace`。重试只能复用相同
请求的操作键；已完成操作返回已有结果，结果不明的操作不会再次执行。
`GET /v1/threads/<id>/commands/<operation_key>` 查询结果；已记录的调用失去执行任务后，
查询会记录明确的结果不确定状态并找回已接纳的回合，不会重新执行回调。`DELETE`
请求取消，包括准入前取消及中断关联的活动回合。
过期注册或工作区变化要求重新选择。命令返回的 prompt 由 Engine 按 Runtime 来源
通过原有回合流程接纳，客户端不能自行再次提交。TUI 和 App 显示插件来源，
并排除普通用户输入的回忆、编辑及重试入口。准入、完成及取消收据保存在已有事件
存储中。插件变更及重连会刷新 App 命令目录，并保留用户修改后的草稿。
会话快照的 `command_receipts` 从原有事件日志投影最近 100 条未提交回合的终态命令，
包含稳定身份、完成时间、状态、结果和插件来源。重新打开会话时显示已有结果，不会
重新执行命令，也不会把这些显示收据加入模型历史。

## 执行前提议

```js
ctx.on('tools/pre-execute', async (exec, next) => {
  if (exec.name !== 'my_plugin_tool') return next()
  if (exec.arguments.label === 'blocked') {
    return { kind: 'deny', reason: 'This label is reserved.' }
  }
  return { kind: 'abstain' }
})
```

监听器接收冻结的 `name`、`callId`、`arguments`、`signal`、`workspace`、`mode`、
`model` 视图，没有 session、agent、tool 或 approval 句柄。`next()` 只表示不干预，
并不执行工具。可返回：

- `{kind:'abstain'}`、`undefined` 或 `null`：不干预。
- `{kind:'deny', reason}`：拒绝调用。
- `{kind:'ask', reason?}`：提出审批要求，由 Rust 的现有模式及权限规则处理。
- `{kind:'revise', input}`：修订为 JSON 对象输入。
- `{kind:'annotate', text}`：补充有边界的上下文。

DSH 的 `allow` 被视为不干预，每个所有者提示一次；不能预先批准。格式错误、异常、
超时及已撤销所有者使调用被拒绝。整个监听器批次有五秒截止时间。Native hooks 先执行，
随后按核心注册顺序执行 mod 监听器；后续监听器会看到已接受的输入修订。拒绝优先，
最后的有效修订决定输入。修订受现有 32 KiB 限制，Rust 重新准备调用并重复准入、策略
和审批检查。理由及上下文沿用现有清理与大小限制。

该桥接作用于 Engine 会话的模型工具、受 gate 管理的 code-mode 调用及 `exec.core`
调用。独立 ACP 执行路径没有 TypeScript 宿主 attachment。`prepend`/`global` 顺序
覆盖、包裹实际执行的中间件、DSH 运行时句柄和执行后结果改写均不支持。

## 提示词片段

`ctx.prompt.registerSection({id, text})` 返回 disposer，贡献带来源的纯文本片段。
Rust 只选择当前 Engine 所需、仍有效并具有已审查 Native 权限的所有者，按所有者/id
稳定排序。完整当前快照通过 user-role 运行时消息交付，替换先前快照；没有片段时明确
撤回先前指令。它不替换系统提示词，也不能指定其他会话的片段。

单个片段最多 4 KiB UTF-8 文本；每个所有者最多 32 KiB、128 个片段；宿主总计最多
128 KiB、1,024 个片段。带来源的最终块也限制为 128 KiB。id 必须以小写字母开头，
只含 `a-z`、`0-9`、`_`、`-`，最多 64 字符。同一所有者重复使用 id 前，必须先撤销
旧注册。停用、撤销、注销或宿主退出都会移除相应片段。

需要当前回合的模型及工作区时，可登记 `{id, text, interpolate: 'model-cwd'}`。
仅支持 `{{model}}` 和 `{{cwd}}`；Rust 在捕获回合提示词时展开一次，普通文本的
花括号保持不变。原始及展开文本均受片段与快照上限约束，捕获和撤回保留精确选定
入口。DSH persona 桥接只贡献 prefix/suffix；替换完整提示词及屏蔽运行时上下文
会被明确拒绝。

## 插件本地状态

`ctx.storage.get(key)`、`set(key, json)`、`delete(key)` 操作 Rust 已指定的 `dataDir`
内的普通 JSON。状态跨重启和代际保留；停用和卸载不会删除该目录。该服务不暴露
会话历史或凭据。所有者开始清理后拒绝访问，也拒绝链接文件、损坏记录和超限写入。

key 最多 128 UTF-8 字节，单个值最多 128 KiB；所有者当前可见记录上限为 4 MiB、
1,024 个 key。每个 key 原子替换；同一宿主内的目录队列串行处理操作。跨进程的同键
写入是最后写入生效，不是事务或跨进程原子增量计数。`mod_counter` 另外串行化自身的
读/改/写队列。损坏状态不会被自动覆盖，应明确选择恢复或删除。

## 注册技能目录

`ctx.mcp.registerServer({serverName, server})` 提议字面量 MCP 定义并返回可重复调用的释放函数。
入口须声明 `inject = ['mcp']`。Rust 的现有目录仍拥有连接、工具准入、权限和认证；
服务不向扩展暴露凭据，也不授予直接进程或网络访问。定义绑定精确的已审查、已选择 Native 入口。
每个 owner 最多 64 个服务器，宿主最多 256 个，每条定义最多 64 KiB。
桥接支持字面量 stdio、streamable HTTP 和 SSE；凭据值、URL 查询数据及不支持的启动/重连控制会被拒绝。
释放入口、变更调用方选择、停用插件或变更已审查字节会撤回定义并取消受影响的调用；不确定的写操作不会重放。

原始 DSH skill-filesystem 桥接复用下述已审查技能根服务。配置必须明确设置
`includeDefaultRoots: false`、`watch: false`，并列出包内相对路径 `customSkillDirs`；
环境中的默认目录及独立 watcher 不受支持。

`ctx.skills.registerRoot({path: 'profiles/review-skills'})` 把已审查包内的技能
交给 Rust 现有的 skill catalog，并返回可重复调用的 disposer。入口的 `inject`
列表需包含 `skills`：

```js
export const inject = ['skills']
export function apply(ctx) {
  ctx.skills.registerRoot({path: 'profiles/review-skills'})
}
```

每个技能放在根目录下的独立子目录中，例如
`profiles/review-skills/quick-check/SKILL.md`。路径相对于插件包，最多 512 个
UTF-8 字节，只能使用正常的 `/` 分隔路径段；绝对路径、链接、空路径段、`.` 和 `..`
会被拒绝。Rust 仅解析已审查字节清单覆盖的文件，复用现有的
[frontmatter 与调用契约](./SKILLS.md#调用与别名元数据)。隐藏子目录会跳过，
含 `SKILL.md` 的技能包会占有其嵌套示例；无效技能或空目录会导致注册被拒绝。
该接口归于已审查 Native 入口，无需额外声明 Skills 组件。

等待注册及已注册目录均占用宿主配额：每个所有者最多 8 个目录，宿主最多 64 个。
Rust 解析每次目录提议时最多读取 128 个候选 `SKILL.md`、4 MiB 原始输入；保留的
指令字段总量每个所有者最多 128 个技能、4 MiB，宿主最多 1,024 个技能、32 MiB。
这些是 catalog 的逻辑配额。相同所有者要重新注册同一路径，需先调用旧 disposer。

清理、停用、撤销信任及宿主退出会从后续发现结果中移除目录。加载时会重新检查
Native 审查凭据和当前注册；排队中的用户选择也会重新检查。进程或宿主重启后，
先前保存的 Native 技能选择需重新选择。已完成的会话历史继续记录当时用过的指令。
该接口不监视文件，不提供配套文件访问，也不授予工具权限；核心审批及沙箱策略
继续生效。

## 请求核心执行工具

工具在直接模型调用、由共享 turn gate 管理本次执行时，可能获得 `exec.core`：

```js
async execute({ path }, exec) {
  if (!exec.core) return { error: 'core handle unavailable' }
  const result = await exec.core.call('read', { path })
  return { content: result.content, isError: result.isError }
}
```

核心调用沿用 allow/deny、hooks、Auto-Review、仓库规则、worker 权限和审批检查。
审批卡片及来源由 Rust 生成，插件不能批准或指定票据。该句柄不出现在激活、timer、
用户命令或 `execute_tools` 中的嵌套扩展工具执行里；调用结束、取消、
插件停用或宿主退出后失效，等待中的审批以取消结果撤回。

嵌套 shell 和网络调用强制要求用户审批，已记住的授权不满足它们；Ask 和
Full Access 都会逐次询问用户，明确的会话拒绝仍然生效。Auto-Review 和 Never
不会打开卡片，会直接拒绝。少数只读、工作区本地工具可免去新的卡片。外层插件工具的批准并不
批准它进一步发起的核心调用，授权按插件构建来源隔离。

不允许递归调用扩展工具、MCP/Computer Use、解释器、agent/workflow/rlm、
`request_user_input`、交互 shell、沙箱提权、会话权限/调度修改工具及其他明确拒绝的
目标。每次扩展工具调用最多发起 50 次核心调用，最多四个并发，一张等待审批卡片；
宿主最多 256 个请求在途。审批等待期间，外层工具截止时钟暂停。完整的拒绝列表和
`CoreCallError` 错误分类见[英文核心调用契约](../EXTENSIONS.md#asking-the-core-to-run-a-tool)。

## 兼容边界

原生 mod 入口、静态导入器及其他客户端的插件格式是不同入口。
兼容的 Claude 插件包仍通过[原生组件适配器](./PLUGINS.md)读取声明组件；这不加载
Claude 的执行循环。Pi 扩展 API 并未被自动适配，需按当前契约手工移植。
DSH 导入器在不执行插件的审查阶段读取 `dsh.bundle.patch`，封存入口、模块及资源的
路径与哈希。已审查的 Native 入口通过同一 Loader 加载选定的组合；`!!js` 表达式
仅在已审查 Native 激活时求值。Bun 不保留带 query/fragment 的模块身份，这类组合
会明确提示选择 Node。缺少 default 的预设目录保持未选中，不会自动选择第一个条目。
损坏或不支持的预设仍显示原因；这不加载 DSH 的 agent 执行循环或浏览器 UI。

目前还没有作者可用的自定义 Ratatui/GPUI 控件及 UI slots。插件不能替换核心的 tools、commands、systemPrompt、审批、会话或凭据服务。
当前功能边界和安装流程见[插件编写指南](./PLUGIN_AUTHORING.md)及[插件包契约](./PLUGIN_BUNDLES.md)。
