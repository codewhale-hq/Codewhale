# 安装插件

> 英文原文：[PLUGINS.md](../PLUGINS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-26。
> 2026-11-07 定点更正：版本范围、目录与固定来源、信任确认、本地更新；未做全文重译。
> 2026-11-12 定点更正：一律称“插件”；未做全文重译。

你添加到 Codewhale 的一切都叫插件。skill、MCP connector（连接器）和 hook 各自是一个插件，
或是插件的一部分，并各有一个种类：Skill、Connector、Hook、Command。你手动设置的 skill 是 Skill 类插件，
你配置的服务器是 Connector 类插件，你配置的 hook 是 Hook 类插件。它们仍在原处管理：终端的
Extensions → Skills、MCP、Hooks，桌面端的 Customize › Skills、Connectors、Hooks；文件、
信任规则和命令（`/skills`、`/mcp`、`/hooks`）都不变。把所有种类放在一个插件列表里是 0.10.3 的计划，
当前版本尚无；目前种类标签只出现在插件包详情里的各个部分上。“Connection”（连接）一词专指模型提供方或
另一个 agent，不是插件的一种。

想做一个插件包，先从[编写你的第一个 Codewhale 插件](./PLUGIN_AUTHORING.md)
和里面可运行的 Skills 示例入手。

本文是 `/plugin install` 这条入口链路的分步说明（v0.9.4，#5182）。
插件包的格式、发现、校验，以及信任/启用生命周期，仍以
[PLUGIN_BUNDLES.md](./PLUGIN_BUNDLES.md) 为准；插件包格式即 `plugin.json`、
兼容的 `kimi.plugin.json`、`.claude-plugin/plugin.json`，或旧版 `plugin.toml`。
本文讲的是文件最初怎么落盘。

版本范围：已发布的 CLI v0.10.2 内置 Computer Use 0.12.1；当前 Engine 源码候选版
已改为单独安装的普通插件。刷新目录或安装新版 Mac 助手，不会替换旧 CLI 的内置包。
完整的版本差异、目录时效和更换固定版本的步骤，见
[英文市场指南](../PLUGIN_MARKETPLACE.md#catalog-freshness-and-pinned-installs)。

`/plugin suggest <task>` 是一个本地、只读的配套命令：它会按名称、关键词、
描述、所含 skill 名、声明的主机，给已安装的插件包与可用目录里的候选项排序，
包括内置目录快照和你用 `/plugin marketplace add` 添加的目录。它还会说明匹配的原因，
并给出下一步：审查、启用，或从目录安装。但它自己从不安装、信任或启用插件包。

## Codewhale 如何推荐插件

Codewhale 在插件上只帮忙，不推销。规则如下：

- **只有一处会主动提示。** 当你发送的任务匹配到某个已安装但闲置的插件，
  或某个你还没有的目录候选项时，可以弹出一条安静的 toast，
  例如 `/plugin trust supabase` 或
  `/plugin marketplace install <catalog> supabase`。你打字时什么都不会出现，
  插件推广内容也不会被塞进你的消息里。
- **只有一个开关。** 关掉 `contextual_tips` 之后，任何地方都不会再出现插件引导。
  必须出现的通知和你自己执行的 `/plugin` 命令照常可用。
- **只有一份配额。** 插件推荐与其他提示共用每会话的引导配额。
  在交互式 TUI 里，模型每个会话可以调用一次 `request_plugin_install`，
  请你审查任务需要的某个插件；第二次调用会失败。
  Exec、ACP 和 runtime-API 会话拿不到这个工具。
- **内置插件从不作为推荐出现。** 先在 `/plugin list` 和 Extensions 检查已有插件。
  在 v0.10.2 上，对 Computer Use 执行目录安装只会提示内置包已存在，不会升级它；
  应审查、信任并启用已有包。安装独立 Computer Use 包的方式适用于当前源码候选版的新布局。
- **只认具体词。** 泛化词（accessibility、browser、chrome、docs、screenshot、
  web、wiki 等）和 Codewhale 自己的词汇（agent、code、data、MCP、model、
  session、tool）永远不触发推荐，所以问 MCP 服务器为什么卡住，不会冒出某个
  厂商的插件。有辨识度的名称、短语和域名照常匹配。匹配器和市场的
  `check-marketplace.mjs` 共用同一份停用词表。
- **只推荐本机跑得起来的。** 插件的 `when.os` 若排除了当前操作系统，就不会被推荐。
- **审查后再操作。** 模型请求的那一行提供 Review，打开 Extensions 中的插件或市场目录。
  点击本身不会安装、信任或启用；仍需选择目录中的显式操作并审查已安装的内容。
- **关闭可以撤销。** 按 Esc 时，如果草稿非空，先清空草稿，然后才隐藏这一行，
  而且只对当前会话生效。“Don't suggest again”是你显式做出的选择，
  会被持久保存。`/plugin dismissals` 会列出这两种；
  `/plugin dismissals reset [<name>]` 可以让被关闭的插件重新出现在推荐里。
- **新插件要靠自己找。** 想找新插件，就看这些文档、`/plugin marketplace list`、
  Extensions，以及下面的浏览器指南。

Codewhale 不会凭空编造远程插件 URL；缺失的插件只会从可用目录里推荐。
磁盘上的插件包有变化时，发送消息时和回合之间仍会弹出 `/plugin reload` 的 toast。

## 浏览器：选一个

操控浏览器有好几种方式。区别在于用谁的浏览器，以及它能看见什么。

| 选项 | 用的是谁的浏览器 | 适合 |
| --- | --- | --- |
| `chrome-devtools` MCP（`/mcp recommendations`） | 它自己驱动的一个 Chrome，可以包含已登录的页面 | DevTools 级别的检查和性能分析 |
| Playwright MCP（`/mcp recommendations`） | 带 `--isolated` 的全新隔离 profile | 不带你身份的脚本化流程和测试 |
| Computer Use 的浏览器工具（审查前关闭） | 它自己启动的浏览器，用独立的 profile | 更宽泛的桌面任务里的浏览器步骤 |
| Chromewhale（开发者预览版，`codewhale-hq/codewhale-plugin-marketplace`） | 你自己的、已经打开的 Chrome profile；以 unpacked 方式加载 | 读取或操作你当前正看的标签页，每次授权一个站点 |

这些都不会主动推荐给你。按任务需要自己添加。

## 来源

`/plugin install <spec>` 的常用来源如下。完整来源和限制见
[英文来源说明](../PLUGINS.md#sources)，包括指定版本的 GitHub 与 npm tarball；
`codewhale install <spec>` 是使用同一安装器的 shell 入口，不需要模型凭据。

```text
/plugin install ./path/to/bundle            # local directory (copied)
/plugin install github:owner/repo           # GitHub archive of the default branch
/plugin install https://example.com/x.tar.gz  # direct tarball URL
```

v1 没有注册表索引，也不做 `git clone`，只拉 tarball。
安装器的大小上限和“不允许符号链接”这两条保证，正靠这一点守住。
下载由逐域名的网络策略把关：未知主机会返回一条“需要审批”的错误，并点出主机名
（先 `/network allow <host>`，再重试）；被拒绝的主机直接中止，不碰磁盘。

拉下来的目录树里必须**有且只有一个**插件包根目录。所谓根目录，
就是放着 `plugin.json`、兼容的 `kimi.plugin.json`、`.claude-plugin/plugin.json`
或旧版 `plugin.toml` 清单的那个目录。Kimi 插件包使用 Codewhale 兼容的 skills、commands、agents 和 MCP 声明时
会被接受；不支持的 Kimi 运行时字段会失败关闭，而不是被静默忽略。
插件包落到用户插件根目录 `~/.codewhale/plugins/<name>/`，
其中的 `<name>` 来自清单里的插件名。

Claude 插件包的元数据放在 `.claude-plugin/plugin.json`，组件放在插件包根目录。
导入器支持 skills、commands、agents，以及内联声明或写在根目录 `.mcp.json` 里的
MCP 服务器（服务器映射可以是平铺的，也可以包在 `mcpServers` 下）。
Claude 的 `http` 传输映射为 Streamable HTTP。
`.claude-plugin/marketplace.json` 目录里的相对来源，会从市场仓库的根目录解析。
整个插件包仍要接受与原生插件相同的审查哈希和路径检查。

远程 MCP 请求头可以只写出凭据的名字，不把凭据本身写进去：authorization 值只要与
`Bearer ${ENV_NAME}` 完全一致，就会转成 `bearer_token_env_var`；请求头值只要与
`${ENV_NAME}` 完全一致，就会转成 `env_headers`。导入过程不读取任何凭据值。
字面量凭据和复合模板会被拒绝。

这是一个兼容子集：hooks、LSP 声明、自定义 MCP 文件路径和
`${CLAUDE_PLUGIN_ROOT}` 展开都会被拒绝，并给出原因；不会装出半个插件。
安装远程 MCP 声明不等于完成它的身份验证。
插件贡献的远程服务器仍保留现有的显式凭据要求；这个导入器不启用插件 OAuth。

## 引导流程

安装从不激活任何东西。命令把文件放好，然后直接把你带进标准的能力审查：

```text
/plugin install github:someone/neat-plugin
→ Installed plugin 'neat-plugin' to ~/.codewhale/plugins/neat-plugin.
  It is disabled and untrusted. Review its requested authority below…
  <full inventory, permissions, MCP authority render>
  /plugin trust neat-plugin <content-hash>.<capability-hash>

/plugin trust neat-plugin <paste the token>   # records the hash-bound receipt
/plugin enable neat-plugin                    # activates for this workspace
```

这里渲染的审查内容和确认 token 与 `/plugin trust <name>` 完全相同。
信任走的是严格的、绑定哈希的回执流程，不是一个仅供参考的标记。
不带 token 的 `/plugin trust <name>` 只显示审查内容，不会授予信任。
必须执行审查打印出的原样确认命令，保留内容哈希和能力哈希这两个完整值；
确认信任后，仍需单独执行 `/plugin enable <name>`。
插件包的内容或声明的能力一旦变化，回执就不再匹配，插件随之失效，直到你重新审查。

## 更新与卸载

```text
/plugin update <name>      # re-download, byte-compare, atomic swap if changed
/plugin disable <name>     # required before uninstall
/plugin uninstall <name>   # deletes the bundle and prunes its state entry
```

- `update` 会重新下载记录在案的来源。字节完全相同就什么都不做；
  有变化的插件包会被原子替换，其信任回执自动失效（哈希不再匹配），
  所以想让插件再次激活，必须重新审查。本地安装则从记录的原来源目录重新暂存副本，
  因此应保留该目录；直接修改已安装副本不会修改原来源。参见
  [本地编写循环](./PLUGIN_AUTHORING.md#4-修改并重新审查)。
- `update` 保留记录中的仓库修订、npm 版本、归档 URL 和包选择路径。
  刷新市场目录不会改变已安装包的来源。要换到更新的固定版本，见
  [英文更换步骤](../PLUGIN_MARKETPLACE.md#changing-an-installed-pin)；
  该步骤仅适用于安装器管理的用户级插件，卸载前应保留原来源与本地修改。
- `uninstall` 拒绝卸载已启用的插件（先停用），删除插件包目录，
  并移除它持久化的信任/启用记录。

## 安全规则

- 每次安装都会带一个 `.installed-from` 来源标记。缺少这个标记的插件包，
  安装器**拒绝覆盖或删除**——手工放到 `~/.codewhale/plugins/` 下的插件包，
  永远不会被覆盖。
- tarball 有大小上限，而且会先解压到一个私有暂存目录。路径穿越（`..`、绝对路径）
  和插件包里的符号链接/硬链接都会被拒绝；等所有检查都通过，
  目标目录才会出现，这一步走的是原子重命名。
- 安装前会拿名称与内置插件包、工作区插件包比对，
  防止优先级更高的插件包被这次安装悄悄遮蔽，也防止它反过来遮蔽这次安装。
- 新装的文件一律**停用且未受信任**；启用只能走上面那套显式的信任审查。
