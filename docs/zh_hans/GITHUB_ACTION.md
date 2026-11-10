# GitHub 审查机器人

> 英文原文：[GITHUB_ACTION.md](../GITHUB_ACTION.md)。
> 最后与英文同步日期（last synced with English revision）：2026-11-10。

根目录的 `action.yml` 使用一个精确指定、带校验和的发行版，运行现有的
`codewhale review` 命令。它需要 Node.js 22+、`gh`、Git，以及 macOS 或 Linux
runner。它只获取 Git 对象，不检出、也不执行 PR 代码。审查覆盖范围、提示词、
路由和发现项（finding）的生成都由引擎（Engine）负责。

使用两个作业（job）：先是分析作业，持有只读的 GitHub 令牌和所选提供商的密钥；
随后是发布作业，持有写令牌，不持有提供商密钥。分析作业导出一份经过校验的
发现项产物（artifact）。发布作业先校验该产物所属的仓库、PR、工作流运行及运行尝试（attempt），
以及当前的 base/head，然后才提交一条 `COMMENT` 审查。它从不批准、从不请求修改、
从不提交代码，也从不运行模型。

自动审查和 `@codewhale review` 评论走同一条审查路径。机器人会发布一份仅供参考的
摘要和经过校验的行内发现项。它从不批准，也从不请求修改。推送分支或创建 PR 的
编码任务仍属于托管智能体；本 Action 负责的是代码审查。

## 搭建你自己的机器人

你需要一个启用了 Actions 的 GitHub 仓库和一个模型 API 密钥。只有在本地生成
工作流时才需要 Node.js 22+ 和 Git；GitHub 的 Ubuntu runner 自带审查所需的工具。
不需要服务器，也不需要 GitHub App。

1. 在一份干净的 Codewhale 源码检出（checkout）中，为你的项目生成工作流：

   ```sh
   node /path/to/Codewhale/scripts/github-action/setup.mjs \
     --directory /path/to/your-project \
     --provider deepseek --model deepseek-flash
   ```

   选择你自己的提供商和精确的模型 ID。生成器会把两个 Codewhale action 都固定到
   该源码检出的完整提交，把 CLI 固定到 `v0.10.2`，并为完成的审查设置 16384 token
   的输出额度。它拒绝覆盖已有的工作流。用 `--print` 查看生成的 YAML，用
   `--version vX.Y.Z` 选择另一个已发布的 CLI，或者配合单独下载的 `setup.mjs`
   使用 `--action-ref FULL_COMMIT_SHA`。`--max-output-tokens N` 用于修改这一显式
   输出额度。推理模型可能在写出发现项之前就耗尽较小的自动额度。生成的工作流使用
   `reasoning-effort: off`：已发布的 v0.10.2 二进制文件对小型审查有一个 4000 token
   的内部上限，这部分额度可能被隐藏的推理过程全部耗光。`--reasoning-effort auto`
   恢复引擎默认值；也可以使用其他显式的推理强度。Action 只会把这一项非机密设置
   写入它隔离的运行配置。提高 Action 的额度并不能突破已发布 CLI 更小的内部上限。
   发布之前仍然需要一次已完成的审查。生成的工作流文件是
   `.github/workflows/codewhale-review.yml`。

2. 只把所选提供商的 API 密钥存为仓库的 Actions 机密（secret）：

   ```sh
   gh secret set DEEPSEEK_API_KEY --repo OWNER/REPO
   ```

   这条命令会提示你输入密钥。这里以 DeepSeek 为例；下文的提供商表格列出了
   其他受支持的机密。切勿把密钥写进 YAML。

3. 把工作流提交到仓库的默认分支并推送。同一仓库内的 PR 在创建或更新时会被
   自动审查。对任何处于打开状态、非草稿的 PR，维护者可以发表一条内容恰好如下的
   评论，或者选择 **Actions → Codewhale PR Review → Run workflow** 并输入 PR 编号：

   ```text
   @codewhale review
   ```

生成的工作流使用 `github-actions[bot]` 身份。若要使用你自己已有的 App，生成时
加上 `--app-login 'YOUR-APP[bot]'`，设置 `CODEWHALE_APP_ID` 变量和
`CODEWHALE_APP_PRIVATE_KEY` 机密，并在该仓库上安装这个 App。使用 App 的工作流
会在发起模型请求之前，先在一个单独的作业里验证安装访问权限。发布作业只申请
contents 读权限和 pull requests 写权限。缺少 App 配置时工作流会失败，不会切换
身份。新建 App 的方法见 [GitHub App 设置](../GITHUB_APP.md)。

`--trigger-phrase @my-bot` 会把提及（mention）改为 `@my-bot review`。提及必须是
一条内容精确匹配的独立评论；引用的文本、任意指令、只发在普通 issue（而非 PR）上的
评论、编辑过的评论以及机器人发表的评论都不会触发模型推理（inference）。对同一仓库内的
PR，新发表的 PR 审查评论（review comment）同样有效。向 GitHub 查询时，最初触发的操作者（actor）和重新运行
工作流的操作者都必须仍具有 write、maintain 或 admin 权限。仅凭关联身份标签
（association）不授予任何访问权限。被删除、被编辑或不匹配的触发评论会被拒绝。

来自 fork 的 PR 不会得到自动的模型审查。生成的工作流选择启用（opt in）对其
惰性（不执行的）diff 的审查，但只限两种途径：经过授权的 issue 评论，或从默认分支发起的
手动运行。它读取
固定的 Git 对象，不检出 PR 文件、不读取项目配置，也不执行候选代码。提供商密钥
属于那个受信任的作业；任何 fork 工作流都拿不到它。在两个 action 上都保持
`allow-forks: false`，即可关闭这一可选功能。

Action 引用（ref）和精确的 CLI 发行版是两个相互独立的固定项。已有的发行标签
包含的是最初版本的审查 action；提及支持和生成器需要一个包含这些功能的提交。
这套设置不会发布浮动的主版本标签。从源码安装并不能证明实时的提供商审查可用。

## 账户配置

Codewhale 托管的 GitHub 审查使用 Codewhale 账户中配置的提供商和模型；如有显式
保存的仓库级覆盖，则以覆盖为准。OrcaRouter 是一个可选的提供商连接，不是审查的
默认值。缺少可用的账户配置时需要先完成设置，系统不会改选其他提供商。

这个独立的 Action 有显式的提供商/模型输入，没有默认提供商。它不会自动获取托管
GitHub App 的设置。请使用你自己的提供商密钥（见下一节）；账户机器密钥（machine
key）路由在后文说明。

## 使用你自己的提供商密钥

选择一个提供商和精确的模型，然后只把它的密钥传给分析作业：

| 提供商 | Actions 机密 |
| --- | --- |
| `deepseek` | `DEEPSEEK_API_KEY` |
| `anthropic` | `ANTHROPIC_API_KEY` |
| `openrouter` | `OPENROUTER_API_KEY` |
| `orcarouter` | `ORCAROUTER_API_KEY` |
| `zai` | `ZAI_API_KEY` |
| `modelstudio-token-plan` | `MODELSTUDIO_API_KEY` |
| `codewhale` | `CODEWHALE_API_KEY`，并需要一条另行验证过的账户机器密钥路由 |

遇到认证、限流或付款失败后，Action 绝不切换凭据。OrcaRouter 是可选的；它宣传的
免费路由仍然需要密钥，并受提供商限额约束。GitHub runner 的使用另有其费用和限额。

[工作流生成器](../../scripts/github-action/setup.mjs)给出了完整、可直接复制的示例。
它使用两个临时（ephemeral）作业和不可变的 action 引用，分析作业只有读权限，只有
发布作业才有写权限。两个作业都不检出仓库代码。发布作业必须使用受信任的 Action
代码，且不得运行发现项产物中的文件。不要把这两个作业合并到持久化 runner 上运行：
分析 runner 上后续的步骤并不构成凭据边界。发布作业只接受当前的工作流运行及
运行尝试；跨工作流的 `workflow_run` 发布未实现。切勿将 `pull_request_target` 与这些
action 一起使用。

### 账户机器密钥

`codewhale` 提供商使用 `CODEWHALE_API_KEY` 和一个精确的 `provider/model` 账户目录
条目。能注册账户，并不说明机器密钥中继访问已具备在 Actions 中使用的资格。除非你
账户的机器密钥路由已经单独验证过，否则请使用显式的 BYOK（自带密钥）。Action 在
使用该路由之前会运行 `codewhale account agent`。

## 上限与证据

| 分析输入 | 默认值 | 含义 |
| --- | --- | --- |
| `version` | 必填 | 精确的已发布 CLI 标签，绝不能是 `latest` |
| `provider` | 必填 | 显式的 BYOK 提供商；或 `codewhale`，用于已验证的账户机器密钥 |
| `model` | 必填 | 精确的模型 ID；账户模式使用 `provider/model` |
| `reasoning-effort` | 引擎自动 | 可选的显式推理强度；生成的 v0.10.2 工作流使用 `off` |
| `max-chars` | 200000 | 每轮完整 diff 的字符数，最大 8388608 |
| `max-passes` | 1 | 完整且按顺序执行的轮数，最大 64 |
| `max-output-tokens` | CLI 自动 | 可选的每轮上限，8192–1000000 |
| `timeout-seconds` | 600 | 模型/发布的截止时间，30–1200 |
| `post` | `true` | 旧版直接发布；生成的工作流使用 `false` |
| `trigger-phrase` | `@codewhale` | 精确的提及名称，后接一条独立的 `review` 命令 |
| `allow-forks` | `false` | 由维护者从默认分支触发、对 fork 的惰性分析；在发布作业上设置相同的值 |

这些上限不保证具体的美元费用。模型价格和推理用量的计算方式各不相同。提高轮数
就意味着授权更多的模型请求。模型推理从不自动重试。启用工作流之前，先设定好预期的
审查范围和提供商的支出控制。

分析的输出为 `outcome`、`receipt`、`findings` 和 `pr-url`。经过清理的回执记录
修订版本、路由、上限、完成计数、发布状态以及上报的 token 用量。它不包含原始的
模型/提供商输出、PR 文本和凭据。缺失的 token 计量保持缺失。在生成回执之前 runner
就已关闭的，不算一次已完成的审查。

`findings` 只在 `post:false` 的完整审查中才会填充。它包含经过校验的结构化审查
文本、证据、文件位置，以及精确的修订版本和工作流身份。未知字段、原始模型输出、
提供商错误和替换代码都会被排除；已知的传入凭据会被脱敏。它仍可能包含私有源码
片段或其他对仓库敏感的文本。只按仓库预期的产物访问权限和保留期限存储它。

发布作业读取当前 PR 的完整文件列表和已有的审查，然后再次核对 base/head。只有
当 GitHub 提供了包含该行的完整有效补丁（patch），且引擎把归属（authorship）记录为由本次变更引入
（introduced）时，发现项才会锚定为行内评论。一条发现项必须具有
`reproduced` 或 `traced` 证据等级，且细节不为空，才会出现在审查中。证据缺失、
为空或仅为疑似（suspected）的发现项留在分析产物里；审查只报告被暂扣的数量。这里
检查的是证据记录，而不是该论断是否属实。针对既有代码的发现项出现在一个折叠区块
中；无法锚定的发现项和未记录归属的发现项出现在正文里。最多 50 条符合条件的
发现项还会以行内评论发布；每一条保留下来的发现项都会留在摘要中。审查正文过大时，
会在任何写入之前失败，而不是悄悄丢弃发现项。

自由形式的模型摘要、合并风险评估和未关联到发现项的建议都留在分析产物中，以免
它们复述一条已被暂扣的指控。发布的摘要链接到分析和覆盖情况，并且从不把空的
发现项列表视为批准。发布回执统计符合证据要求的 `findings`，并单独报告
`withheld_findings`。这些规则适用于独立的发布作业；旧版 `post:true` 沿用固定
版本 CLI 自己的策略。

发布作业只在 `reviewer-login` 发布的审查上识别它自己的审查/发现项指纹。完全相同
的审查重跑会被跳过。规范化后的标题/描述、严重程度、位置都相同且文件内容未变的
发现项不会重复发布，即使其他无关文件发生了变化也是如此。文件内容改变后，可能会
产生新的发现项。这是确定性的去重，而不是语义相似度判断；措辞不同的发现项仍可能
重复出现。并发必须保持按 PR 串行，因为 GitHub 没有为审查创建提供原子性的幂等键。

CLI 会校验 diff 是否被完整覆盖。它从固定的 Git blob 中读取有限的片段；它不执行
PR 中的测试，也不检查任意未改动的调用方。“Reviewed clean”（审查无问题）的意思是
一次已完成的审查报告了零个问题，并不能证明这个 PR 没有 bug。

## 结果与恢复

| 结果 | 含义 / 下一步 |
| --- | --- |
| `reviewed_clean` | 分析完整，报告零个问题 |
| `reviewed_with_findings` | 分析完整；发现项仅供参考 |
| `published` | 发布作业已确认在捕获的 head 上发布了一条 `COMMENT` 审查 |
| `already_published` | 配置的发布者名下已存在匹配的审查或发现项 |
| `configuration_missing` | 检查发行版/模型、密钥、上限、产物和工作流身份 |
| `failed` | 没有完整的分析/发布回执；查看安全的原因码 |
| `incomplete` | 覆盖不完整；调整审查范围或上限 |
| `publication_uncertain` | 重试前先检查 PR；写入可能已经成功 |
| `superseded` | base 或 head 已变化；请分析当前修订版本 |
| `not_eligible` | 未获批准的 fork、草稿、已关闭的 PR、不受支持的事件、过期评论或缺少写权限；不会运行模型，也不会写入 |

失败会使这个可选作业失败；它是否作为必需的合并检查，由仓库所有者的策略决定。
提供商失败并不是对 PR 的否定结论。不会另外发布失败评论。

修复设置后，带上 PR 编号运行整个工作流。在新的运行尝试中只重跑发布步骤，
会拒绝旧的产物；这可以防止悄悄发布来自另一次执行的产物包。重新启用一个被禁用的
工作流不会重放旧事件。旧版 `post:true` 保留 CLI 的直接发布行为，享受不到独立
发布作业的跨运行去重和写令牌隔离。
