# Codewhale 中文术语表

本表用于 TUI 语言包、`docs/zh_hans/` 和网站中文词典的编辑与复核。
产品名称与行为以当前英文源码和[产品语气](VOICE.md)为准；本表统一译法，不另行定义产品权限或运行时契约。

## 翻译规则

- 保留命令、配置键、环境变量、文件路径、按键名、模型名、提供商名称、JSON 字段、错误码和 `{named}` 占位符。不要把可执行示例中的标识符翻译成中文。
- 产品模式保留 `Plan`、`Work`、`Operate`，权限档位保留 `Ask`、`Auto-Review`、`Full Access`，团队功能保留 `Fleet`。文档首次出现时可以加中文释义；界面已有本地化标签时使用对应的统一译法。
- 面向用户使用“智能体”；只有说明父子关系、代码结构或协议时才使用“子智能体”。`worker`、`subagent`、`lane` 等标识符按源码保留。
- 简体与繁体分别审校，不把字符转换当作完整翻译。繁体语言包仍待母语者审核；键和占位符检查通过不代表自然度或地区用语已经合格。
- 同一概念在同一文档内使用同一译法。根据上下文区分模型 token 与认证令牌、会话与对话、凭据与回执、智能体与网络代理。
- 译文不能扩大权限、削弱拒绝条件、把未知说成成功，或把尚未实现、仅本地验证的行为说成已上线。
- 英文源码与实现冲突时，应修正共同来源并同步译文，不以翻译悄悄消解冲突。

## 产品与交互

| 英文 | 简体中文 | 繁体中文 | 说明 |
| --- | --- | --- | --- |
| Codewhale | Codewhale | Codewhale | 保留产品名。 |
| Agent | 智能体 | 代理 | 网络 proxy 另译为代理服务器／代理伺服器。 |
| Sub-agent | 子智能体 | 子代理 | 仅在需要说明父子关系时使用。 |
| Fleet | Fleet | Fleet | 首次可释为“智能体团队”／“代理團隊”；不要以“舰队”或泛称“团队”替代产品名。 |
| Plan / Work / Operate | Plan / Work / Operate | Plan / Work / Operate | `Act`、`Agent`、`yolo` 等旧配置值按兼容性契约保留，不把它们当作当前模式名称。 |
| Ask | 询问 | 詢問 | 权限档位的中文释义。 |
| Auto-Review | 自动审核 | 自動審核 | 文档保留英文产品标签。 |
| Full Access | 完全访问 | 完整存取 | 不代表绕过显式拒绝、仓库保护规则或执行沙箱。 |
| Coordinator | 协调者 | 協調者 | 协调 Fleet 的会话。 |
| Advisor | 顾问 | 顧問 | 提供第二意见的模型。 |
| Tasks panel | 任务面板 | 任務面板 | 替代面向用户的 Workbar／工作栏名称；配置键不变。 |
| Settings | 设置 | 設定 | 设置界面；简体中的配置文件内容称“配置”。 |
| Thinking | 思考 | 思考 | 思考强度设置；模型类别中的 reasoning model 仍可译为推理模型。 |
| Constitution | 宪章 | 憲章 | 不用“宪法”“教义”等法律或宗教类比；宪章不能授予运行时权限。 |
| Repository law | 仓库保护规则 | 儲存庫保護規則 | 保留源码中的 `repo_law` 等标识符。 |
| Allow once | 仅允许本次 | 僅允許一次 | 审批选项。 |
| Allow for this conversation | 在本次对话中允许 | 在本次對話中允許 | conversation 与 session 不混用。 |
| Always for this command | 此命令始终允许 | 此命令一律允許 | 只保存这条命令，不扩大到其他仓库。 |
| Don't allow | 不允许 | 不允許 | 拒绝是执行结果，不必作为按钮名称。 |
| Stop | 停止 | 停止 | 不把停止回合误译为删除会话。 |

## 运行时与数据

| 英文 | 简体中文 | 繁体中文 |
| --- | --- | --- |
| Engine | 引擎 | 引擎 |
| Runtime | 运行时 | 執行階段 |
| Session | 会话 | 工作階段 |
| Conversation | 对话 | 對話 |
| Turn | 回合 | 回合 |
| Provider | 提供商 | 供應商 |
| Model route | 模型路由 | 模型路由 |
| Context window | 上下文窗口 | 上下文視窗 |
| Compaction | 压缩 | 壓縮 |
| Token（模型用量） | token | token |
| Token（认证） | 令牌 | 權杖 |
| Skill | 技能 | 技能 |
| Plugin | 插件 | 外掛 |
| Extension（编辑器等） | 扩展 | 擴充功能 |
| Hook | 钩子 | 掛鉤 |
| Workflow | 工作流 | 工作流程 |
| Receipt（执行或审查记录） | 回执 | 回執 |
| Approval（名词） | 审批 | 核准 |
| Approve（动词） | 批准 | 核准 |
| Authorization | 授权 | 授權 |
| Sandbox | 沙箱 | 沙箱 |
| Credential | 凭据 | 憑證 |
| API key | API 密钥 | API 金鑰 |
| Trust | 信任 | 信任 |
| Memory（智能体笔记） | 记忆 | 記憶 |
| Memory（RAM） | 内存 | 記憶體 |
| Repository / repo | 仓库 | 儲存庫 |
| Worktree | 工作树 | 工作樹 |
| Usage（token／费用） | 用量 | 用量 |
| Diff | diff | diff |

“回执”指关于执行结果的记录，不是付款收据。“凭据”指认证材料，不是执行证据。

## 地区用语

| 英文 | 简体中文 | 繁体中文 |
| --- | --- | --- |
| Server | 服务器 | 伺服器 |
| File | 文件 | 檔案 |
| Project | 项目 | 專案 |
| Software / program | 软件／程序 | 軟體／程式 |
| Default | 默认 | 預設 |
| Network | 网络 | 網路 |
| Support | 支持 | 支援 |
| Message | 消息 | 訊息 |
| Queue | 队列 | 佇列 |
| Terminal | 终端 | 終端機 |
| Clipboard | 剪贴板 | 剪貼簿 |
| Paste | 粘贴 | 貼上 |
| Byte | 字节 | 位元組 |
| Command | 命令 | 指令 |

## 复核与同步

1. 对照当前英文文案和调用处，检查否定、范围、默认值、失败行为、权限与费用含义。
2. 检查键集、占位符、命令、配置键、代码示例和相对链接；网站词典更改后重新导出本地 catalog。
3. 文档同步日期只反映实际完成的源码对照，不代表真实提供商调用、各平台实机验证或发布验收。
4. 将未完成的母语审校、布局检查或平台验证明确留在状态记录中，不用机械检查的通过代替它们。
