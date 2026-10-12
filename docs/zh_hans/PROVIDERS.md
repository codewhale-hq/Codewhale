# 提供商注册表

> 英文原文：[PROVIDERS.md](../PROVIDERS.md)。
> 最后与英文同步日期（last synced with English revision）：2026-09-29。
> ChatGPT 登录相关内容于 2026-10-02 按当前实现更新。
> 2026-10-06 补齐已捕获身份、账号切换/限额及 OrcaRouter PKCE；媒体能力见 [MEDIA_MODELS.md](MEDIA_MODELS.md)。

本注册表描述已接入当前 Codewhale 代码库的提供商行为。它刻意保持保守：随附条目仅限于代码已知的提供商 ID、配置键、认证路径、base URL、模型解析和能力元数据。

DeepSeek 仍是默认提供商，但 `ProviderKind::ALL` 中的每个条目都是一等公民、可选的提供商路由。`ALL` 是目录/选择器所用的清单——每个厂商一个身份。双线协议方言种类（`*Anthropic`，例如 `deepseek-anthropic`）和 Model Studio 套餐变体保留在枚举中用于 serde 和 `provider_for_kind`，但刻意**不**作为目录行：套餐是主提供商配置（`crates/config/src/provider_kind.rs:221-226`）上的 `mode`/`base_url`，方言则是 `wire = openai|anthropic`。托管路由、通用 OpenAI 兼容端点、OpenAI Codex/ChatGPT 路由、原生 Anthropic 以及本地运行时，都在所选提供商/模型/base URL 上运行同一个终端 harness。

经普通 Chat Completions 访问的主机是普通的具名提供商，而不是 `ProviderKind`：枚举变体只留给不同的*线协议*（Anthropic Messages、Codex Responses、Google 思维签名）。这类主机都是一张 `[providers.<name>]` 表，包含 base URL、模型和密钥环境变量（见 `docs/CONFIGURATION.md`）；`/provider` 与 `/setup` 为此保留了“粘贴 Base URL 和密钥”的路径。可选模型来自实时的 `GET /v1/models` 加上 Codewhale 目录，而不是编译进程序的固定名单（#5350、#6289）。

已知可用的主机。这些主机以内置描述符行的形式随附在 `crates/config/assets/provider_descriptors.json` 中（由 `crates/config/src/descriptors.rs` 编译进程序）：每一行说明如何连接该主机，即线协议、base URL、密钥环境变量和别名，而模型 ID 仍以实时的 `GET /v1/models` 与 Codewhale 目录为准；示例模型只是引导用的提示。信任此处任何值之前，请先对照厂商自己的文档核实：

| 主机 | Base URL | 示例模型 | API 密钥环境变量 |
| --- | --- | --- | --- |
| SenseNova | `https://token.sensenova.cn/v1` | `deepseek-v4-flash` | `SENSENOVA_API_KEY` |
| Baseten | `https://inference.baseten.co/v1` | `deepseek-ai/DeepSeek-V4-Pro` | `BASETEN_API_KEY` |
| Groq | `https://api.groq.com/openai/v1` | `llama-3.3-70b-versatile` | `GROQ_API_KEY` |
| Cerebras | `https://api.cerebras.ai/v1` | `llama-3.3-70b` | `CEREBRAS_API_KEY` |
| Command Code | `https://api.commandcode.ai/provider/v1` | `deepseek/deepseek-v4-flash` | `COMMAND_CODE_API_KEY` |
| 阿里云百炼 Model Studio（DashScope） | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | `qwen3.8-flash` | `DASHSCOPE_API_KEY` |
| AICraft | `https://aicraftapi.com/v1` | `claude-4.6-sonnet`；DeepSeek / Claude / Gemini / Qwen / GLM / MiniMax / Doubao 系列 | `AICRAFT_API_KEY` |
| Tsubasa | `https://api.tsubasa.sh/v1` | `tsubasa-pro`, `tsubasa-fast`（32,768 token 上下文） | `TSUBASA_API_KEY` |
| Yolo-Auto | `https://yolo-auto.com/v1` | `qwen3.8-flash`；`yolo` / `yolo-small` | `YOLO_AUTO_API_KEY` |
| Cheaper Inference | `https://api.cheaperinference.com/v1` | `gpt-5.4-mini`、`claude-sonnet-5`、`gemini-3.1-pro` | `CHEAPER_INFERENCE_API_KEY` |

AICraft 的 OpenAI 兼容端点提供 DeepSeek、Anthropic Claude、Google Gemini、Qwen、GLM、MiniMax 和 Doubao 的模型 ID。权威来源是带上你的密钥请求 `GET https://aicraftapi.com/v1/models` 的结果：请从该列表中选择模型，而不是从上表中选。
Tsubasa 只实现了 `GET /v1/models` 和 Chat Completions。它的两个公开模型 ID 共用 32,768 token 的上下文，小于 Codewhale 对未知模型假定的 128,000 token，因此保存后请在该路由上设置：
`codewhale config set providers.tsubasa.context_window 32768`。
Cheaper Inference 是 OpenAI 兼容网关，模型 ID 不带前缀（例如 `gpt-5.4-mini`、`claude-sonnet-5`），以带密钥请求 `GET https://api.cheaperinference.com/v1/models` 的结果为准；价格随模型和路由而变，请以提供商的实时目录为准。
OpenCode Zen 和 OpenCode Go 是一等提供商路由，配置方式与下文其他提供商相同；它们不在上表之列。在 `/provider` 中，直接输入即可筛选列表（未绑定行操作的字母）；`Ctrl+T` 探测所选行的 `/models`，只记录可达性（2xx 并不代表模型可用）。

需要保持同步的来源：

- `crates/config/src/lib.rs` —— 共享的提供商 ID、默认值、环境变量优先级。
- `crates/config/assets/provider_descriptors.json` —— 内置的 OpenAI 兼容主机描述符（即上文的已知可用主机表）。
- `crates/tui/src/config.rs` —— TUI 提供商 ID、提供商能力元数据以及提供商特定的环境变量处理。
- `crates/agent/src/lib.rs` —— `codewhale model resolve` 使用的静态 `ModelRegistry`（以及 `[model]` 配置面）。`codewhale model list` 则改为读取解析后的 provider lake，因此一次列表就能同时看到内置、实时与已配置的行。
- `config.example.toml` 和 `docs/CONFIGURATION.md` —— 面向用户的配置示例和环境变量参考。
- `scripts/check-provider-registry.py` —— 对规范提供商 ID、实时 TUI 提供商 ID、TOML 表名、静态注册表行和文档化默认值的漂移检查。

## 已捕获的提供商身份

名称、标签、别名及旧线协议标签来自 `provider_descriptors.json`。`ProviderKind`
仍拥有 Rust 内部的凭据、协议与地区区别。配置路由同时捕获 kind 与精确表键；
名为 `openai` 的自定义表仍是自定义路由，不会因名称而获得 OpenAI 凭据或协议规则。
自定义键区分大小写。

DeepSeek 中国路由保留三个表示：已发布 TUI serde 包装的 `deepseek_c_n`、捕获路由
ID `deepseek-cn`、配置叶子 `deepseek_cn`。固有 kind 是 DeepSeek，表与端点仍独立。

旧自定义路由缺少新增的 provider ID 时，只能凭经过验证的根 `base_url` 迁移回执
恢复到活动 `providers.custom` 表。私有回执绑定解析后的表版本；仅表、仅 profile、
冲突或后来替换的表不足以建立缺失身份，显式空 ID 会被拒绝。写入复用现有锁内配置
变更与撤销比较，在同一锁内获得新迁移回执，核对捕获表后才改叶子。

在途请求保留捕获身份、端点及凭据版本。健康查询不能仅因旧请求使用过同类认证，
就把不透明凭据引用当作已就绪。

## 提供商选择

没有已保存的模型、没有 `default_text_model`，也没有 `CODEWHALE_MODEL` 或提供商专用的模型环境变量时，全新安装会在 DeepSeek 提供商上运行 `deepseek-flash`。优先级依次是：当前提供商已配置的默认值，然后是它的目录默认值——因此显式设置的 `default_text_model` 不会被随附目录中排在最前的模型悄悄覆盖。

无需安装新的 Codewhale 版本即可刷新模型目录：

```sh
codewhale models --update
codewhale models --update --provider openai
codewhale models --provider openai --json
```

`models --update`（也可写作 `--refresh`）会更新共享的 Models.dev 元数据，并用每个已配置提供商各自的凭据调用其现有的 `/models` 端点。`--provider ID` 把刷新限定在该提供商，包括具名的自定义端点。它不发起任何推理请求，也不会更改已保存的提供商或模型。命令行传入的 API 密钥只限于当前提供商；本次调用中其他路由会被报告为已跳过。

不带 `--update` 的 `models` 会列出当前提供商已保存的目录，不发起提供商请求。ChatGPT 路由会先在本地验证所选注册，再读取该账户的缓存目录。刷新成功后，结果保存在 Codewhale 的目录文件夹下，供模型/提供商选择器使用。缓存文件按提供商身份和端点划分；刷新失败时会保留原有行。文本输出报告来源、最近一次成功获取的时间（Unix 秒）和新鲜度。`--update --json` 会额外给出每个来源的回执和汇总计数；部分失败时，会在写完这些回执后返回非零退出码。普通的 `models --json` 仍保持模型数组格式。随附的或已配置的回退项并不能证明账户可以使用列出的每个模型。

### 使用 ChatGPT 登录

`openai-codex` 现在使用 OpenAI 官方的[开源应用登录流程](https://developers.openai.com/siwc/token-sharing-open-source)。先登录，再刷新当前账户的模型目录：

```sh
codewhale auth chatgpt
codewhale models --update --provider openai-codex
codewhale --provider openai-codex
```

登录通过系统浏览器、回环回调和 PKCE 完成。Codewhale 验证返回的身份与套餐使用权限，把注册信息和可续期令牌保存在自己的受保护凭据存储中。身份登录不等于套餐授权；未授予 `chatgpt.tokens.use.direct` 权限时，推理会停止。使用 `codewhale auth chatgpt-revoke` 退出 Codewhale 的登录会话。当前实验版只保留一个活动账户注册，尚未提供已保存账户的切换界面。

在 Codewhale 内运行 `/auth chatgpt` 会重新授权当前账户和工作区，并更新正在运行的会话；普通重新登录会沿用已签发的客户端 ID，验证返回的仍是同一账户。要更换账户或工作区，请在终端运行 `CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt`，刷新新账户的模型目录，再重启已打开的 Codewhale 会话。只有通过完整验证的新授权才会替换现有登录。浏览器若登录了错误的账户，可先退出该账户，或用隐私窗口打开打印出的登录链接。

`codewhale auth status --provider openai-codex` 和 `codewhale auth list` 会在 ID 令牌含有邮箱时显示账户标签；套餐和工作区信息只在相应字段存在时显示。标签用于识别账户，不能证明套餐权限；没有邮箱标签也不代表已经退出登录。状态检查只读取本地凭据，不向签发方发起请求：访问令牌已过期但仍存有刷新令牌时，会被视为结构上可用，这不保证续期一定成功。登录成功后，会显示已知的账户标签，并说明是否替换了先前的登录。

模型目录使用已获授权的令牌请求 `GET https://api.openai.com/v1/models`；推理使用公共端点 `POST https://api.openai.com/v1/responses`。选择器保留官方显示名称和顺序，仅列出 `visibility: list` 的模型。目录不含凭据，并按已验证的签发方、签发的客户端 ID 和账户主体隔离；模型列表不能证明上下文窗口或思考档位。目录缺失、无效或超过 24 小时时，选择器不会列出任何账户模型。Codex CLI 导入令牌和旧版进程令牌变量不能授权这条官方路由。

符合条件的请求消耗 ChatGPT 套餐额度或点数。Plus 的五小时额度在各应用之间共享，不会为每个应用单独分配；这项五小时限制不适用于 Pro。应用还可能有各自的使用上限。可在 [ChatGPT 设置 → 使用情况](https://chatgpt.com/settings/usage) 查看额度和授权。额度耗尽时，Codewhale 不会自动换用 API 密钥或其他提供商。

这是面向本地开源应用的预览集成，不读取 ChatGPT 对话历史。请求使用 `store: false` 和 `stream: true`，会话历史由 Codewhale 保存。此路由不支持 OpenAI 托管的图像生成、文件搜索、Code Interpreter、原生电脑操作、MCP/连接器或 Responses `tool_search`；Codewhale 自己的工具使用受支持的函数调用。参见[预览限制](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)。付费或远程托管应用需要走[商业合作申请流程](https://openai.com/form/sign-in-with-chatgpt-interest/)；完成本地集成不代表已获得商业批准。

ChatGPT 预览版一次只请求一个函数调用。

规范的提供商 ID 是 `ProviderKind::ALL`（`crates/config/src/provider_kind.rs`）的全部条目，按该顺序排列：

`deepseek`, `nvidia-nim`, `openai`, `atlascloud`, `wanjie-ark`, `volcengine`,
`openrouter`, `orcarouter`, `xiaomi-mimo`, `novita`, `fireworks`, `siliconflow`, `arcee`,
`siliconflow-CN`, `moonshot`, `sglang`, `vllm`, `ollama`, `ollama-cloud`, `huggingface`,
`together`, `qianfan`, `openai-codex`, `anthropic`, `openmodel`, `zai`,
`stepfun`, `minimax`, `deepinfra`, `sakana`, `longcat`, `opencode-go`,
`opencode-zen`, `meta`, `xai`, `mistral`, `telecomjs`, `modelstudio-token-plan`, `modelscope`,
`google`, `edenai`, `zenmux`, `csdn`, `concentrate`, `codewhale` 和 `custom`。

`deepseek-anthropic` *不在*此列表中——它是 `deepseek` 的线协议方言，通过 `wire = "anthropic"` 访问，而不是一个可单独选择的路由。

可以使用以下任一界面选择提供商：

- CLI：`codewhale --provider <id>`
- TUI：`/provider <id>` 或提供商选择器
- 环境变量：`CODEWHALE_PROVIDER=<id>`；`DEEPSEEK_PROVIDER=<id>` 是旧别名
- 配置：`provider = "<id>"`

`deepseek-cn`、`deepseek_china`、`deepseekcn` 和 `deepseek-china` 被接受为 `deepseek` 的旧别名。它们不会选择不同的官方主机；DeepSeek 在全球使用同一个官方 API 主机。

`deepseek_anthropic`、`deepseek-claude` 和 `deepseek_claude` 选择 `deepseek-anthropic`，这是 DeepSeek 的可选路由，在 `https://api.deepseek.com/anthropic` 上使用 Anthropic Messages API。它保留正常的 DeepSeek API 密钥路径，但使用 `x-api-key` 加上 `anthropic-version: 2023-06-01`，而不是 Bearer 认证。如果密钥已经存在于官方 DeepSeek Harness（`dsh`）的 `$DSH_HOME/.credentials.yaml` 中，请使用 `codewhale auth external-consent --provider deepseek --mode read-only` 授予只读访问权限。Codewhale 从不写入该文件，只读取 `DEEPSEEK_API_KEY`。

`huggingface`、`hugging-face`、`hugging_face` 和 `hf` 都选择 Hugging Face Inference Providers 路由。这是用于聊天/推理的 OpenAI 兼容路由器路径，不是 Hub 浏览、模型卡检查、上传或工件导出。

`telecomjs`、`telecom-js`、`telecom_js`、`telecomjs-cn` 和 `tokenhub` 都选择 TelecomJS TokenHub 路由。其经过认证的 `/models` 目录按密钥隔离，并与所有其他提供商的实时快照保持隔离。

新的共享配置写入 `~/.codewhale/config.toml`。出于兼容性，仍然会读取现有的 `~/.deepseek/config.toml` 文件。

### 旧版 Antigravity 墓碑记录

Antigravity 不是 Codewhale 的提供商，无法被选择或运行。已有的旧版 Antigravity 提供商状态只会被识别为不可运行的迁移墓碑记录。运行 `codewhale auth clear --provider antigravity` 只会清除 Codewhale 自己保存的旧版配置和授权元数据；这不会登出、撤销、读取或以其他方式改动任何官方 Google 或 Antigravity 会话。如需使用 Gemini，请选择受支持的 `google` 提供商，并提供 `GEMINI_API_KEY`。

### 线协议兼容性

提供商选择是显式的。诸如 `deepseek-ai/...`、`deepseek/...`、`qwen/...` 或 `arcee-ai/...` 之类的模型字符串前缀，是所选提供商下的、由提供商拥有的线协议 ID 或目录命名空间提示。它不是提供商切换，绝不能被视为该路由是 DeepSeek、OpenRouter 或任何其他提供商的证据。

使用 `provider = "<id>"`、`CODEWHALE_PROVIDER=<id>` 或 `codewhale --provider <id>` 设置路由。使用 `CODEWHALE_MODEL`、提供商特定的模型环境变量、顶层 `default_text_model` 或 `[providers.<table>].model` 设置请求模型。使用 `CODEWHALE_BASE_URL`、提供商特定的 base URL 环境变量或 `[providers.<table>].base_url` 设置端点。使用 `codewhale auth set --provider <id>`、`[providers.<table>].api_key` 或列出的提供商环境变量设置认证。

| 提供商 ID | TOML 表 | 线协议 | 认证环境变量 |
| --- | --- | --- | --- |
| `deepseek` | `[providers.deepseek]` | 模型感知：默认使用 Responses（`deepseek-flash`）；Chat Completions（`deepseek-v4-pro`） | `DEEPSEEK_API_KEY` |
| `deepseek-anthropic` | `[providers.deepseek_anthropic]` | Anthropic Messages | `DEEPSEEK_API_KEY` |
| `nvidia-nim` | `[providers.nvidia_nim]` | OpenAI Chat Completions | `NVIDIA_API_KEY`, `NVIDIA_NIM_API_KEY` |
| `openai` | `[providers.openai]` | OpenAI Chat Completions | `OPENAI_API_KEY` |
| `atlascloud` | `[providers.atlascloud]` | OpenAI Chat Completions | `ATLASCLOUD_API_KEY` |
| `wanjie-ark` | `[providers.wanjie_ark]` | OpenAI Chat Completions | `WANJIE_ARK_API_KEY`, `WANJIE_API_KEY`, `WANJIE_MAAS_API_KEY` |
| `volcengine` | `[providers.volcengine]` | OpenAI Chat Completions | `VOLCENGINE_API_KEY`, `VOLCENGINE_ARK_API_KEY`, `ARK_API_KEY` |
| `openrouter` | `[providers.openrouter]` | OpenAI Chat Completions | `OPENROUTER_API_KEY` |
| `xiaomi-mimo` | `[providers.xiaomi_mimo]` | OpenAI Chat Completions | `XIAOMI_MIMO_TOKEN_PLAN_API_KEY`, `MIMO_TOKEN_PLAN_API_KEY`, `XIAOMI_MIMO_API_KEY`, `XIAOMI_API_KEY`, `MIMO_API_KEY` |
| `novita` | `[providers.novita]` | OpenAI Chat Completions | `NOVITA_API_KEY` |
| `fireworks` | `[providers.fireworks]` | OpenAI Chat Completions | `FIREWORKS_API_KEY` |
| `siliconflow` | `[providers.siliconflow]` | OpenAI Chat Completions | `SILICONFLOW_API_KEY` |
| `arcee` | `[providers.arcee]` | OpenAI Chat Completions | `ARCEE_API_KEY` |
| `siliconflow-CN` | `[providers.siliconflow_cn]` | OpenAI Chat Completions | `SILICONFLOW_API_KEY` |
| `moonshot` | `[providers.moonshot]` | OpenAI Chat Completions | `MOONSHOT_API_KEY`, `KIMI_API_KEY` |
| `sglang` | `[providers.sglang]` | OpenAI Chat Completions | `SGLANG_API_KEY` |
| `vllm` | `[providers.vllm]` | OpenAI Chat Completions | `VLLM_API_KEY` |
| `ollama` | `[providers.ollama]` | 本地 OpenAI 兼容 Chat Completions | `OLLAMA_API_KEY`（可选；仅用于需要认证的本地路由） |
| `ollama-cloud` | `[providers.ollama_cloud]` | 托管 OpenAI 兼容 Chat Completions | `OLLAMA_CLOUD_API_KEY`, `OLLAMA_API_KEY` |
| `huggingface` | `[providers.huggingface]` | OpenAI Chat Completions | `HUGGINGFACE_API_KEY`, `HF_TOKEN` |
| `modelscope` | `[providers.modelscope]` | OpenAI Chat Completions | `MODELSCOPE_API_KEY` |
| `together` | `[providers.together]` | OpenAI Chat Completions | `TOGETHER_API_KEY` |
| `qianfan` | `[providers.qianfan]` | OpenAI Chat Completions | `QIANFAN_API_KEY`, `BAIDU_QIANFAN_API_KEY` |
| `openai-codex` | `[providers.openai_codex]` | OpenAI Responses | 官方“使用 ChatGPT 登录”（`codewhale auth chatgpt`），需要已验证的 Codewhale 自有套餐授权 |
| `anthropic` | `[providers.anthropic]` | Anthropic Messages | `ANTHROPIC_API_KEY` |
| `openmodel` | `[providers.openmodel]` | Anthropic Messages | `OPENMODEL_API_KEY` |
| `zai` | `[providers.zai]` | OpenAI Chat Completions | `ZAI_API_KEY`, `Z_AI_API_KEY` |
| `stepfun` | `[providers.stepfun]` | OpenAI Chat Completions | `STEPFUN_API_KEY`, `STEP_API_KEY` |
| `minimax` | `[providers.minimax]` | OpenAI Chat Completions | `MINIMAX_API_KEY` |
| `deepinfra` | `[providers.deepinfra]` | OpenAI Chat Completions | `DEEPINFRA_API_KEY`, `DEEPINFRA_TOKEN` |
| `sakana` | `[providers.sakana]` | OpenAI Chat Completions | `FUGU_API_KEY`, `SAKANA_API_KEY` |
| `longcat` | `[providers.longcat]` | OpenAI Chat Completions | `LONGCAT_API_KEY` |
| `opencode-go` | `[providers.opencode_go]` | OpenAI Chat Completions | `OPENCODE_GO_API_KEY` |
| `opencode-zen` | `[providers.opencode_zen]` | 模型感知：OpenAI Responses、Anthropic Messages 或 OpenAI Chat Completions | `OPENCODE_ZEN_API_KEY`, `OPENCODE_API_KEY` |
| `meta` | `[providers.meta]` | OpenAI Chat Completions | `META_MODEL_API_KEY`, `MODEL_API_KEY` |
| `telecomjs` | `[providers.telecomjs]` | OpenAI Chat Completions | `TELECOMJS_API_KEY` |
| `xai` | `[providers.xai]` | OpenAI Chat Completions | `XAI_API_KEY` |
| `mistral` | `[providers.mistral]` | OpenAI Chat Completions | `MISTRAL_API_KEY` |
| `google` | `[providers.google]` | OpenAI Chat Completions（官方 Gemini OpenAI 兼容路由；在工具调用时捕获并回放思维签名） | `GOOGLE_API_KEY`, `GEMINI_API_KEY` |
| `edenai` | `[providers.edenai]` | OpenAI Chat Completions | `EDENAI_API_KEY` |
| `zenmux` | `[providers.zenmux]` | OpenAI Chat Completions | `ZENMUX_API_KEY` |
| `csdn` | `[providers.csdn]` | OpenAI Chat Completions | `CSDN_API_KEY` |
| `concentrate` | `[providers.concentrate]` | OpenAI Responses（`/v1/responses`） | `CONCENTRATE_API_KEY` |
| `codewhale` | `[providers.codewhale]` | 模型感知：OpenAI Chat Completions（`/v1/chat/completions`）或 Anthropic Messages（`/v1/messages`），由账户目录按模型决定 | `CODEWHALE_API_KEY` |
| `modelstudio-token-plan` | `[providers.modelstudio_token_plan]` | OpenAI Chat Completions | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` |
| `modelstudio-token-plan-anthropic` | `[providers.modelstudio_token_plan_anthropic]` | Anthropic Messages | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` |
| `modelstudio-coding-plan` | `[providers.modelstudio_coding_plan]` | OpenAI Chat Completions | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` |
| `modelstudio-coding-plan-anthropic` | `[providers.modelstudio_coding_plan_anthropic]` | Anthropic Messages | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` |

每条路由的默认 base URL 和模型列在下方随附的提供商表中。上面的线协议值源自 `crates/config/src/provider.rs`：`ChatCompletions` 是默认值，`openai-codex` 覆盖为 `Responses`；`deepseek-anthropic`、`anthropic` 和 `openmodel` 覆盖为 `AnthropicMessages`；`opencode-zen` 根据所选模型的精选供应解析协议；而 `deepseek` 是模型感知的——出厂默认的 `deepseek-flash`（以及旧版 `deepseek-v4-flash`）走 Responses 端点，`deepseek-v4-pro` 仍走 Chat Completions。

## 认证与环境变量规则

对于托管提供商，`codewhale auth set --provider <id>` 会为该提供商保存一个 API 密钥。API 密钥环境变量是已保存配置和密钥环凭据之后的回退输入；显式的进程级 `--api-key` 在该次启动中仍然优先。

对于 base URL 和模型选择，优先使用：

- 活动提供商对应的 `CODEWHALE_BASE_URL` / `CODEWHALE_MODEL`。
- 下方列出的提供商特定 base URL/模型环境变量。
- `DEEPSEEK_BASE_URL`、`DEEPSEEK_MODEL` 和 `DEEPSEEK_DEFAULT_TEXT_MODEL` 作为旧别名。

非本地的 `http://` base URL 会被拒绝，除非设置了 `DEEPSEEK_ALLOW_INSECURE_HTTP=1`。环回 HTTP URL 允许用于自托管运行时。

## 自定义 DeepSeek 兼容端点

大多数自定义的 DeepSeek 兼容部署都可以使用现有的提供商 ID。不要创建 `[providers.deepseek_custom]`；提供商表名是固定的。相反，选择最接近的随附路由并覆盖其端点/模型：

- DeepSeek 兼容托管 API：保持 `provider = "deepseek"` 并设置 `[providers.deepseek].base_url` 加上 `[providers.deepseek].model`，或者使用 `DEEPSEEK_BASE_URL` 和 `DEEPSEEK_MODEL` 启动。
- 通用 OpenAI 兼容网关：使用 `provider = "openai"`，设置 `[providers.openai].base_url` 加上 `[providers.openai].model`，或者使用 `OPENAI_BASE_URL` 和 `OPENAI_MODEL` 启动。
- 多个具名 OpenAI 兼容网关，或者你想从 AgentProfile 固定的本地路由，可以使用自定义表，例如 `[providers.lm-studio] kind = "openai-compatible"`，并用 `provider = "lm-studio"` 或配置文件 `provider = "lm-studio"` 选择它。
- 本地 OpenAI 兼容运行时：使用 `provider = "vllm"`、`"sglang"` 或 `"ollama"`，并配上匹配的提供商特定 base URL/模型值。

DeepSeek 兼容主机的用户配置示例：

```toml
provider = "deepseek"

[providers.deepseek]
api_key = "YOUR_API_KEY"
base_url = "https://your-provider.example/v1"
model = "deepseek-ai/DeepSeek-V4-Pro"
```

通用网关的用户配置示例：

```toml
provider = "openai"

[providers.openai]
api_key = "YOUR_GATEWAY_API_KEY"
base_url = "https://gateway.example/v1"
model = "your-deepseek-compatible-model"
```

阿里云百炼（Bailian / DashScope）自 v0.9.4 起是一等提供商，具有两个套餐配置：Token 套餐（个人/团队）和 Coding 套餐。两个套餐都暴露 OpenAI 兼容的 Chat Completions 端点和 Anthropic 兼容的 Messages 端点。

**Token 套餐**（个人和团队共享同一个 AP-Southeast 端点）：

```toml
provider = "modelstudio-token-plan"

[providers.modelstudio_token_plan]
api_key = "YOUR_MODELSTUDIO_API_KEY"
# base_url 默认为 https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1
model = "qwen3.8-max"   # 或 qwen3.8-max-preview | qwen3.7-plus | qwen3.7-max |
                        #    qwen3.6-flash | deepseek-v4-pro | deepseek-v4-flash-0731 |
                        #    glm-5.2
```

**Coding 套餐**（独立的国际端点）：

```toml
provider = "modelstudio-coding-plan"

[providers.modelstudio_coding_plan]
api_key = "YOUR_MODELSTUDIO_API_KEY"
# base_url 默认为 https://coding-intl.dashscope.aliyuncs.com/v1
model = "qwen3.8-max"
```

**Anthropic 兼容方言** —— 两个套餐还暴露原生 Anthropic Messages 路径。使用 `-anthropic` 提供商后缀选择它：

```toml
provider = "modelstudio-token-plan-anthropic"

[providers.modelstudio_token_plan_anthropic]
api_key = "YOUR_MODELSTUDIO_API_KEY"
# base_url 默认为 https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic
model = "qwen3.8-max"
```

从[百炼控制台](https://bailian.console.aliyun.com/)创建或复制一个 Model Studio API 密钥。该 API 密钥在上述四个提供商 ID 之间共享；只有 base URL 和线协议不同。

**思考/推理。** 推理会在 TUI 的 Thinking 视图上以两种方言呈现，依据 Model Studio 的[深度思考文档](https://www.alibabacloud.com/help/en/model-studio/deep-thinking)。

在 OpenAI 兼容路由上，顶层控制是**路由和模型特定**的，Codewhale 失败关闭：只有当配置的 `base_url` 是官方阿里 Chat Completions 主机（`*.maas.aliyuncs.com/compatible-mode/v1`，包括工作区级主机，或 `coding-intl.dashscope.aliyuncs.com/v1`）时，它们才会被发送。同一提供商 ID 上的自定义 `base_url` 会剥离 `thinking`、`enable_thinking`、`preserve_thinking` 和 `reasoning_effort`，因此任意的 OpenAI 兼容网关永远不会收到阿里的方言。在已验证的主机上：

- **混合模型**（`qwen3.7-*`、`qwen3.6-*`、`deepseek-v4*`、`glm-*`、`kimi-k2.6*`）获得 `enable_thinking`：`off` 时为 `false`，否则为 `true`。
- **仅思考模型** —— `qwen3.8-max`（编目为 `thinking: always_on`）、`qwen3.8-max-preview`（提供 effort/预算选项，无开关）和 `kimi-k2.7-code` —— 完全没有启用/禁用开关。发送这样的开关，最好的情况也只是被忽略。
- 对文档记录为接受它的模型（`qwen3.7-max`/`-plus`、`qwen3.6-max-preview`/`-plus`/`-flash`、`kimi-k2.6*`、`kimi-k2.7-code`）发送 `preserve_thinking`，这样下一回合会保留助手的思考轨迹。
- `reasoning_effort` 只对有文档记载的分档（档位阶梯）的两个系列发送 —— `deepseek-v4*` 和 `glm-5`/`5.1`/`5.2` —— 映射到 `high` 或 `max`。

推理以 `delta.reasoning_content` 流式返回。在后续回合中，只有上述 `preserve_thinking` 模型和仅思考模型的推理内容才会重放给提供商；`deepseek-v3.1`、`deepseek-v3.2` 和 `glm-*` 历史保持剥离，直到实时确认 DashScope 接受输入消息中的 `reasoning_content`。（`deepseek-v4*` 无论如何都会重放——DeepSeek 思考模式契约要求每个提供商都这样做。）

在 Anthropic 兼容路由上，思考使用[Anthropic 兼容 Messages API](https://www.alibabacloud.com/help/en/model-studio/anthropic-api-messages)中记录的 `{"type":"enabled","budget_tokens":N}` / `{"type":"disabled"}` 形态，`budget_tokens` 由 effort 级别推导而来。

由 Model Studio 提供的 DeepSeek（`deepseek-v4-pro`、`deepseek-v4-flash-0731`）和 GLM（`glm-5.2`）模型是提供商范围的，不会与第一方 DeepSeek 或智谱/Z.ai 路由冲突。Model Studio 没有发布 `glm-5.3` 条目，因此 Codewhale 不会在此路由上提供它。
按量付费的 workspace-id 模板化尚未进入内置提供商；在该后续跟进添加它之前，请为那个套餐使用自定义提供商条目。

证书损坏或被拦截的私有网关应使用带受信 CA 包的 `SSL_CERT_FILE`。旧版 `insecure_skip_tls_verify = true` 键仍然会被解析，以便 `codewhale doctor` 可以报告过时配置，但提供商客户端会拒绝它，而不是跳过 TLS 证书验证。

将 `provider`、`api_key` 和 `base_url` 保存在用户配置或进程环境中。项目本地配置覆盖层刻意不能设置这些键，因此仓库无法静默地把提示或凭据重定向到另一个端点。

## 本地模型（DS4、Ollama、vLLM、SGLang）

自托管的 OpenAI 兼容运行时是一等路由，默认无密钥——只有当你的服务器需要密钥时才设置 API 密钥。启动你的运行时，然后用 `--provider` / `/provider` 或配置表把 Codewhale 指向它。

| 运行时 | 默认 base URL | 默认模型 | Base URL 覆盖 |
| --- | --- | --- | --- |
| `ollama` | `http://localhost:11434/v1` | 来自 `GET /v1/models` 的实时标签（刷新前：`unknown`） | `OLLAMA_BASE_URL` |
| `vllm` | `http://localhost:8000/v1` | `deepseek-ai/DeepSeek-V4-Pro` | `VLLM_BASE_URL` |
| `sglang` | `http://localhost:30000/v1` | `deepseek-ai/DeepSeek-V4-Pro` | `SGLANG_BASE_URL` |

### DS4（DwarfStar）

[DS4](https://github.com/antirez/ds4/tree/84cc882352757baf628a1776badf7cc54d584e28) 通过 OpenAI 兼容 API 在本地提供 DeepSeek V4 Flash 和 Pro。启动 DS4，然后打开 Codewhale 预填好的、无密钥设置表单：

```bash
./ds4-server --ctx 100000 --kv-disk-dir /tmp/ds4-kv --kv-disk-space-mb 8192
codewhale
# 在 Codewhale 中：/setup provider ds4
```

检查预填的路由并按 Enter 保存。该预设为 100,000 token 的上下文做预算，以匹配该启动命令，并默认使用 Flash 兼容别名。用 `codewhale doctor --probe-local` 显式检查本地路由。

DS4 在服务器启动时加载实际的 GGUF。它的 `deepseek-v4-flash` 和 `deepseek-v4-pro` API id 是兼容别名；更改 `/model` 不会替换驻留的模型。要运行 Pro，请下载受支持的 Pro 权重并按 DS4 的描述启动 `ds4-server -m <pro.gguf> ...`。每当服务器的 `--ctx` 值变化时，更新 `context_window`。

等效配置是：

```toml
provider = "ds4"

[providers.ds4]
kind = "openai-compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "deepseek-v4-flash"
auth_mode = "none"
context_window = 100000
```

Codewhale 为 DS4 复用它现有的 OpenAI 兼容传输和 DeepSeek 推理/工具调用整形。它不会虚构 API 密钥、把 API 别名与已加载的 GGUF 混淆，或静默切换到托管的 DeepSeek 路由。固定的 DS4 [agent-client 契约](https://github.com/antirez/ds4/blob/84cc882352757baf628a1776badf7cc54d584e28/README.md#agent-client-usage)记录了 `/v1` 上的 Chat Completions、DeepSeek 思考重放、流式用量、`max_tokens` 以及无严格工具模式；Codewhale 遵循这些确切的路由事实，而不是从通用网关继承不受支持的能力。面向模型的行为的主要来源是 DeepSeek 官方的[思考模式](https://api-docs.deepseek.com/guides/thinking_mode)、[工具调用](https://api-docs.deepseek.com/guides/tool_calls)和[Chat Completion](https://api-docs.deepseek.com/api/create-chat-completion)契约。固定的 [DeepSeek Harness 适配器](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/llm/llm-deepseek/README.md)只是次要的实现交叉检查；它不是 API 契约。

### Ollama

```bash
ollama serve          # 如果尚未运行
ollama pull <model>   # 例如 deepseek-v4-flash，或你喜欢的任何标签
codewhale --provider ollama --model <model>
```

提供商提示的模型名会原样发送，因此 `--model qwen3:8b` 对 Ollama 已拉取的任何标签都有效。

### Ollama Cloud

Ollama Cloud 是单独的托管提供商。它使用经过认证的 OpenAI 兼容 `/v1/chat/completions` 路由，默认为 `gpt-oss:120b`：

```toml
provider = "ollama-cloud"

[providers.ollama_cloud]
base_url = "https://ollama.com/v1"
model = "gpt-oss:120b"
```

在 [Ollama 账户设置](https://ollama.com/settings/keys)中创建密钥，然后运行 `codewhale auth set --provider ollama-cloud`。对于环境认证，`OLLAMA_CLOUD_API_KEY` 优先于 Ollama 官方的 `OLLAMA_API_KEY`。`OLLAMA_CLOUD_BASE_URL` 和 `OLLAMA_CLOUD_MODEL` 覆盖 Cloud 默认值；任意的提供商自有模型 ID 原样通过。本地 `ollama` 仍然是单独的、默认无密钥的提供商。

兼容性是只读且存在于内存中的：已发布的配置如果选择了 `provider = "ollama"` 且精确规范化的 `[providers.ollama] base_url = "https://ollama.com/v1"` 元组，则运行时按 `ollama-cloud` 处理。只有那个精确元组才能回退到旧版 `ollama` 密钥槽。Codewhale 不会重写配置、复制或删除密钥、迁移相邻路径，也不会让显式的 `ollama-cloud` 路由消耗旧版密钥槽。

### vLLM

```bash
vllm serve <model> --port 8000
# 或：python -m vllm.entrypoints.openai.api_server --model <model> --port 8000
codewhale --provider vllm --model <model>
```

vLLM 的 OpenAI 兼容服务器默认监听 8000 端口，与 Codewhale 的 `VLLM_BASE_URL` 匹配。

### SGLang

```bash
python -m sglang.launch_server --model-path <model> --port 30000
codewhale --provider sglang --model <model>
```

SGLang 的默认端口 30000 与 Codewhale 的 `SGLANG_BASE_URL` 匹配。

### 在配置中固定本地路由

```toml
provider = "ollama"       # 或 "vllm" / "sglang"

[providers.ollama]
model = "qwen3:8b"        # 默认是 deepseek-v4-flash
# base_url 默认为 http://localhost:11434/v1
```

打印工具调用 JSON 但没有线协议标记的本地模型：参见[当本地模型输出工具 JSON 时](#当本地模型输出工具-json-时)。

## 凭据链接

提供商设置界面使用与 onboarding、`/provider`、`/links`、设置回执和 doctor 输出相同的类型化凭据元数据。缺失 URL 是有意为之：本地、仅 OAuth 和用户定义的路由会显示其支持的配置路径，而不是猜测厂商页面。

| 提供商 ID | 凭据或控制台链接 |
| --- | --- |
| `deepseek`, `deepseek-anthropic` | [DeepSeek API 密钥](https://platform.deepseek.com/api_keys) |
| `nvidia-nim` | [NVIDIA NIM API 密钥](https://build.nvidia.com/settings/api-keys) |
| `openai` | [OpenAI API 密钥](https://platform.openai.com/api-keys) |
| `atlascloud` | [Atlas Cloud API 密钥](https://atlascloud.ai/docs/en/api-keys) |
| `wanjie-ark` | [Wanjie MaaS APIKEY 文档](https://docs.wanjiedata.com/maas/maas-openapi-v1.html) |
| `volcengine` | [Volcengine Ark API 密钥](https://console.volcengine.com/ark/apiKey) |
| `openrouter` | [OpenRouter 密钥](https://openrouter.ai/settings/keys) |
| `xiaomi-mimo` | [Xiaomi MiMo Token 套餐](https://platform.xiaomimimo.com/token-plan) |
| `novita` | [Novita 密钥管理](https://novita.ai/en/settings/key-management) |
| `fireworks` | [Fireworks API 密钥](https://fireworks.ai/api-keys) |
| `siliconflow` | [SiliconFlow 全球 API 密钥](https://cloud.siliconflow.com/account/ak) |
| `siliconflow-CN` | [SiliconFlow 中国 API 密钥](https://cloud.siliconflow.cn/account/ak) |
| `arcee` | [Arcee API 密钥指南](https://docs.arcee.ai/other/create-your-first-api-key) |
| `moonshot` | [Kimi API 平台密钥](https://platform.kimi.ai/console/api-keys) 或 [Kimi Code 会员控制台](https://www.kimi.com/code/console) |
| `zai` | [Z.ai 模型 API](https://z.ai/model-api) |
| `stepfun` | [StepFun 开放平台](https://platform.stepfun.ai/) |
| `minimax`, `minimax-anthropic` | [MiniMax 接口密钥](https://platform.minimax.io/user-center/basic-information/interface-key) |
| `huggingface` | [Hugging Face token](https://huggingface.co/settings/tokens) |
| `modelscope` | [ModelScope API 密钥](https://modelscope.cn/my/settings/token) |
| `deepinfra` | [DeepInfra API 密钥](https://deepinfra.com/dash/api_keys) |
| `together` | [Together API 密钥](https://api.together.ai/settings/api-keys) |
| `qianfan` | [百度云访问密钥](https://console.bce.baidu.com/iam/#/iam/accesslist) |
| `anthropic` | [Anthropic API 密钥](https://console.anthropic.com/settings/keys) |
| `openmodel` | [OpenModel 控制台](https://console.openmodel.ai/)（[认证指南](https://docs.openmodel.ai/en/docs/getting-started/authentication)） |
| `openai-codex` | 通过 `codewhale auth chatgpt` 使用官方 ChatGPT 登录，消耗符合条件的套餐额度或点数，令牌由 Codewhale 自己保管。`openai` API 密钥路由单独计费；旧版 Codex 导入凭据不能授权此路由。 |
| `sglang`, `vllm` | 本地 OpenAI 兼容端点默认无密钥；仅当服务器需要时才配置密钥。 |
| `ollama` | 本地 Ollama 默认无密钥；仅当本地服务器需要时才配置密钥。 |
| `ollama-cloud` | 创建[Ollama API 密钥](https://ollama.com/settings/keys)，用 `codewhale auth set --provider ollama-cloud` 保存，或按该优先级顺序设置 `OLLAMA_CLOUD_API_KEY` / `OLLAMA_API_KEY`。 |
| `sakana` | [Sakana AI API 密钥](https://console.sakana.ai/api-keys)（[入门](https://console.sakana.ai/get-started)） |
| `longcat` | [美团 LongCat 平台](https://longcat.chat/platform) |
| `opencode-go` | [OpenCode Go](https://opencode.ai/docs/go/) |
| `opencode-zen` | [OpenCode Zen](https://opencode.ai/docs/zen/) |
| `meta` | [Meta Model API](https://developer.meta.com/ai/) |
| `telecomjs` | [TelecomJS TokenHub](https://aigw.telecomjs.com/) |
| `xai` | [xAI 控制台](https://console.x.ai/) 获取 API 密钥、Codewhale 自有的设备登录，或明确同意的只读 Grok CLI 凭据。 |
| `mistral` | [Mistral 控制台 (la Plateforme)](https://console.mistral.ai/api-keys) |
| `google` | [Google AI Studio](https://aistudio.google.com/apikey) —— Codewhale 使用官方 Gemini OpenAI 兼容端点，绝不读取 Google OAuth 文件。 |
| `edenai` | [Eden AI API 密钥](https://app.edenai.run/settings/api-keys) |
| `zenmux` | [ZenMux API 密钥](https://zenmux.ai/platform/pay-as-you-go) |
| `csdn` | [CSDN 星图控制台](https://ai.csdn.net/workbench/api-key)——`glm_for_coding` 套餐路由请选择 Coding Plan 密钥类型；通用密钥按量计费。文档：[Coding Plan](https://ai.csdn.net/coding-plan)。 |
| `concentrate` | [Concentrate 控制台](https://concentrate.ai/) → API Keys → Create API Key（一个 Universal API 密钥）；文档：[API 简介](https://concentrate.ai/docs/api-reference/introduction)。仅支持自带密钥（BYOK）——密钥保存在本地的密钥存储中。 |
| `modelstudio-token-plan`, `modelstudio-token-plan-anthropic`, `modelstudio-coding-plan`, `modelstudio-coding-plan-anthropic` | [阿里云百炼 Model Studio（百炼控制台）](https://bailian.console.aliyun.com/) —— 创建或复制 Model Studio API 密钥。 |
| `codewhale` | [Codewhale 账户设置](https://app.codewhale.net/settings?section=api)——创建带 `models:infer` 权限范围的 API 密钥，或运行 `codewhale account api-keys create --name <name> --use`。 |
| `custom` | 设置该具名提供商的 `base_url` 和 `api_key_env` 或 `api_key`；不存在规范的厂商凭据页面。 |

对于 Kimi，官方[快速入门](https://platform.kimi.ai/docs/overview)引导用户登录、打开 **API 密钥**、创建并复制密钥，并保守秘密。Codewhale 直接链接到该控制台并接受复制的密钥。它从不探测或冒充 `kimi_cli`/`kimi_code_cli`；一等 Kimi OAuth 仍然受阻于厂商注册的 Codewhale 身份。

### 订阅账号与使用上限

Codewhale 为每个订阅提供商保存一个自有登录，与浏览器、ChatGPT/Grok 应用及
Codex/Grok CLI 分开。成功的新登录替换旧自有授权；不提供多个已保存 ChatGPT
账号的选择器。

- `codewhale auth status --provider openai-codex`（或 `xai`）及 `auth list`
  只读取本地状态。有 ID token 邮箱声明时显示账号标签，有套餐/工作区声明时才显示
  相应资料；标签不证明权益。有效授权也可能没有标签。访问令牌过期但有刷新令牌
  仍可结构性使用；存储无效、缺失或过期且无刷新令牌则不可用。
- `/auth chatgpt` 更新当前会话；从 shell 执行 `codewhale auth chatgpt` 后需重启
  已打开会话。普通登录复用所选账号/工作区已签发的 client ID，并验证账号一致。
  `CODEWHALE_CHATGPT_OAUTH_NO_PROMPT=1` 可在发行者拒绝 `prompt=login` 时省略它。
- 从 shell 执行 `CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt` 更换
  账号/工作区，随后刷新模型目录并重启会话；只有完整验证的新授权替换活动授权。
  xAI 使用 `/auth xai-device` 或 shell `codewhale auth xai-device`；设备页批准
  浏览器当前账号，必要时先退出浏览器账号或使用隐私窗口。
- `subscription_sharing_usage_limit_exceeded` / `subscription_sharing_usage_unavailable`
  以及其他订阅配额错误给出当前请求账号（已知时）与恢复指引；不会自动重试终态
  错误或悄悄换计费路由。xAI OAuth 配置若实际使用 API key，则不显示登录恢复指引。
- 官方 ChatGPT 套餐路由只使用所选、已验证的 Codewhale 自有授权。
  `OPENAI_CODEX_ACCESS_TOKEN`、`CODEX_ACCESS_TOKEN` 或获准读取的 Codex CLI
  token 都不能覆盖或授权该路由。成功登录回执不会打印 token。

### 外部 CLI 凭据授权

归另一 CLI 所有的凭据文件默认禁用。没有显式授权，提供商发现、设置、路由、`auth status` 和 doctor 不会 stat、读取、刷新、联系身份提供商或重写 Codex、Grok、Kimi 或未来的外部凭据文件。

Codewhale 目前为 Codex CLI 和 Grok CLI 保留按精确路径、提供商隔离的**只读**授权。Codex 授权仅用于检查旧版凭据，不能授权官方 ChatGPT 套餐路由；该路由必须通过 `codewhale auth chatgpt` 登录。

```bash
# 仅检查旧版 Codex 凭据，不授权 ChatGPT 套餐请求。
codex login
codewhale auth external-consent --provider openai-codex --mode read-only

grok login
codewhale auth external-consent --provider xai --mode read-only

codewhale auth status --provider openai-codex
codewhale auth external-revoke --provider openai-codex
```

当外部 CLI 使用自定义位置时，传入 `--path /absolute/path/to/auth.json`。授权会持久化提供商、外部所有者、精确绝对路径和授权模式版本。之后的环境变量更改不会把该权威重定向到另一个文件。只读授权不会刷新令牌、联系身份或发现服务，也不会重写外部文件。xAI 等受支持路由在显式选中后可以使用外部令牌；官方 ChatGPT 套餐路由忽略 Codex 导入令牌，只接受自己的已验证授权。外部令牌过期时，请求会停止并提示重新登录。Doctor 在不打开凭据文件的情况下报告结构性的授权/配置状态，并且始终不执行变更。

`managed` 保留给未来的提供商特定保留适配器。v0.9.1 在文件或网络 I/O 之前拒绝它，因为还没有经过审查的适配器能够安全地保留每一个未知的外部模式字段。Codewhale 启动的 xAI 设备登录改为原子地激活一个 Codewhale 自有的、名为 `$CODEWHALE_HOME/credentials/xai-auth-<generation>.json` 的 generation，只把该已验证的基本名存入配置，并撤销任何 Grok 文件授权。被取代的 generation 只在新配置指针提交之后清理。
Kimi 仍然仅支持 API 密钥；对 Kimi 的外部授权被拒绝。

官方 DeepSeek Harness（`dsh`）是第三个只读凭据所有者：`codewhale auth external-consent --provider deepseek --mode read-only` 授予对 `$DSH_HOME/.credentials.yaml`（或 `~/.dsh/.credentials.yaml`）中 `DEEPSEEK_API_KEY` 的精确路径读取访问，Codewhale 从不写入、刷新或将其加载到进程环境中。这与 DSH *harness* 集成（`codewhale integrations dsh …`，参见 [INTEGRATIONS_DSH.md](../INTEGRATIONS_DSH.md)）是分开的，后者在两个方向上都不接触凭据：它把 Codewhale 的路由身份固定进一个 `--patch` 覆盖层，并让 DSH 解析自己的密钥。

## 随附的提供商

| 提供商 ID | TOML 表 | 认证环境变量 | Base URL 环境变量和默认值 | 默认或静态模型 | 备注 |
| --- | --- | --- | --- | --- | --- |
| `deepseek` | `[providers.deepseek]` | `DEEPSEEK_API_KEY` | `CODEWHALE_BASE_URL` / `DEEPSEEK_BASE_URL`；默认 `https://api.deepseek.com/beta` | `deepseek-flash`（出厂默认；V4.1 Flash，无版本号的 ID）, `deepseek-v4-pro`, `deepseek-v4-flash`，实验性的 `deepseek-v4-flash-vision-exp`；视觉别名 `flash-vision`, `deepseek-v4flashvisionexp`；兼容别名 `deepseek-chat`, `deepseek-reasoner` | 一等默认提供商。线上 Pro 后端标记为 `DeepSeek-V4-Pro-0813`；可调用的 API ID 仍是 `deepseek-v4-pro`。Beta URL 启用严格工具模式、chat 前缀补全和 FIM 补全。文档中记载的 V4 路由可以通过单独的、有界的 Responses 请求使用提供商原生的网页搜索；兼容的自定义端点不会继承这一能力。显式设置 `https://api.deepseek.com` 或 `/v1` 可退出仅限 beta 的功能。出厂默认的 `deepseek-flash` 使用 Responses API（自 2026-07-31 的 Flash 生产环境更新起，这是 DeepSeek 文档中记载的 Codex 式集成路径）；仅适用于 Chat 的控制项（`thinking` 开关和严格工具的 `/beta` 路由）只作用于 Chat Completions 路由，而 `deepseek-v4-pro` 在其已宣布的 Responses 推出之前仍走 Chat Completions。思考强度遵循 DeepSeek 文档中记载的“请求值到实际值”映射：`minimal`/`low` 落到 `low`，`medium`/`xhigh` 落到 `high`，`max`/`ultra` 落到 `max`；`off` 关闭思考（Chat 上为 `thinking: {"type":"disabled"}`，Responses 上为 `reasoning.effort: "none"`）。该实验性视觉 ID 于 2026-08-21 在已认证的 `/models` 名单中被观察到，仅在直连的 Chat Completions 路由上声明支持图像输入。它的限制、推理和工具调用标志暂时继承自 Flash；价格未知，本次发布工作中也没有做过付费的图像往返测试。 |
| `deepseek-anthropic` | `[providers.deepseek_anthropic]` | `DEEPSEEK_API_KEY` | `DEEPSEEK_ANTHROPIC_BASE_URL`；默认 `https://api.deepseek.com/anthropic` | `deepseek-v4-pro`, `deepseek-v4-flash`；兼容别名 `deepseek-chat`, `deepseek-reasoner` | 面向 Anthropic Messages 线协议的 DeepSeek 显式选择加入路由。使用 `/v1/messages`、`x-api-key` 和 `anthropic-version: 2023-06-01`。默认 Chat Completions 路径请保持 `provider = "deepseek"`。 |
| `nvidia-nim` | `[providers.nvidia_nim]` | `NVIDIA_API_KEY`, `NVIDIA_NIM_API_KEY` | `NVIDIA_NIM_BASE_URL`, `NIM_BASE_URL`, `NVIDIA_BASE_URL`；默认 `https://integrate.api.nvidia.com/v1` | `deepseek-ai/deepseek-v4-pro`, `deepseek-ai/deepseek-v4-flash` | 通过 NVIDIA NIM 托管的 DeepSeek V4。TUI 配置路径接受 `NVIDIA_NIM_MODEL`。 |
| `openai` | `[providers.openai]` | `OPENAI_API_KEY` | `OPENAI_BASE_URL`；默认 `https://api.openai.com/v1` | `gpt-5.6`（默认）, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` | 通用 OpenAI 兼容路由，其内置端点和回退目录均以 OpenAI 原生为准。[GPT-5.6 系列](https://developers.openai.com/api/docs/models/gpt-5.6-sol)使用 OpenAI 文档中记载的 1.05M 上下文、128K 最大输出和推理级别。自定义网关仍可自由选择由网关自己提供的显式模型。接受 `OPENAI_MODEL`。 |
| `atlascloud` | `[providers.atlascloud]` | `ATLASCLOUD_API_KEY` | `ATLASCLOUD_BASE_URL`；默认 `https://api.atlascloud.ai/v1` | 默认 `deepseek-ai/deepseek-v4-flash`；选择 AtlasCloud 时显式的 `vendor/model-id` 值直通 | OpenAI 兼容托管路由。TUI 配置路径接受 `ATLASCLOUD_MODEL`，静态 `ModelRegistry` 保留 DeepSeek V4 回退行，提供商提示的 CLI 模型 ID 按原样发送给 AtlasCloud。当前提供商自有的模型列表与定价请查看 Atlas Cloud 自己的目录或 Coding Plan 页面。 |
| `wanjie-ark` | `[providers.wanjie_ark]` | `WANJIE_ARK_API_KEY`, `WANJIE_API_KEY`, `WANJIE_MAAS_API_KEY` | `WANJIE_ARK_BASE_URL`, `WANJIE_BASE_URL`, `WANJIE_MAAS_BASE_URL`；默认 `https://maas-openapi.wanjiedata.com/api/v1` | `deepseek-reasoner` | OpenAI 兼容托管路由。接受 `WANJIE_ARK_MODEL`, `WANJIE_MODEL` 和 `WANJIE_MAAS_MODEL`。 |
| `volcengine` | `[providers.volcengine]` | `VOLCENGINE_API_KEY`, `VOLCENGINE_ARK_API_KEY`, `ARK_API_KEY` | `VOLCENGINE_BASE_URL`, `VOLCENGINE_ARK_BASE_URL`, `ARK_BASE_URL`；默认 `https://ark.cn-beijing.volces.com/api/coding/v3` | `DeepSeek-V4-Pro`, `DeepSeek-V4-Flash` | 火山引擎/Volcengine Ark OpenAI 兼容编码端点。接受 `VOLCENGINE_MODEL` 和 `VOLCENGINE_ARK_MODEL`。 |
| `openrouter` | `[providers.openrouter]` | `OPENROUTER_API_KEY` | `OPENROUTER_BASE_URL`；默认 `https://openrouter.ai/api/v1` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash`；近期大模型 ID 包括 `arcee-ai/trinity-large-thinking`, `minimax/minimax-m3`, `xiaomi/mimo-v2.5-pro`, `qwen/qwen3.6-flash`, `qwen/qwen3.6-35b-a3b`, `qwen/qwen3.6-max-preview`, `qwen/qwen3.6-27b`, `qwen/qwen3.6-plus`, `google/gemma-4-31b-it`, `z-ai/glm-5.1`, `z-ai/glm-5.2`, `moonshotai/kimi-k2.7-code`, `moonshotai/kimi-k2.6` | 附加的开放模型路由层。它不是 DeepSeek 的替代品；它让用户在选择 OpenRouter 时把受支持的模型 ID 路由过去。 |
| `orcarouter` | `[providers.orcarouter]` | `ORCAROUTER_API_KEY` | `ORCAROUTER_BASE_URL`；默认 `https://api.orcarouter.ai/v1` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash`；路由别名 `orcarouter/auto`；近期大模型 ID 镜像 OpenRouter 的命名空间目录 | [OrcaRouter](https://www.orcarouter.ai) OpenAI 兼容聚合网关。与 OpenRouter 共享命名空间的 `vendor/model` 线协议模型格式和 DeepSeek 模型集，因此 OpenRouter 的 base-URL 和模型规范化规则适用。接受 `ORCAROUTER_MODEL`。提供商别名：`orcarouter`, `orca_router`, `orca`。 |
| `xiaomi-mimo` | `[providers.xiaomi_mimo]` | `XIAOMI_MIMO_TOKEN_PLAN_API_KEY`, `MIMO_TOKEN_PLAN_API_KEY`, `XIAOMI_MIMO_API_KEY`, `XIAOMI_API_KEY`, `MIMO_API_KEY` | `XIAOMI_MIMO_BASE_URL`, `MIMO_BASE_URL`, `XIAOMI_MIMO_MODE`, `MIMO_MODE`；默认 `https://token-plan-sgp.xiaomimimo.com/v1` | 聊天：`mimo-v2.5-pro`, `mimo-v2.5-pro-ultraspeed`, `mimo-v2.5`；语音/TTS：`mimo-v2.5-tts`, `mimo-v2.5-tts-voicedesign`, `mimo-v2.5-tts-voiceclone`, `mimo-v2-tts` | 小米 MiMo OpenAI 兼容聊天补全路由。`mimo-v2.5-pro` 和 `mimo-v2.5` 可以使用文档中记载的提供商原生网页搜索插件；ultraspeed、语音/TTS 以及自定义兼容路由不会继承它。Token Plan 密钥（`tp-...`）默认使用 `api-key` 认证和 token-plan 端点；按量付费模式使用标准 API 密钥（`sk-...`）和 `https://api.xiaomimimo.com/v1`。它发送 `max_completion_tokens` 并用 MiMo 的 `thinking` 字段进行推理控制。Token Plan 成本/用量基于额度/配额；在小米暴露可靠的余额 API 之前，Codewhale 将其显示为未知。`codewhale speech` / `tts` 使用 TTS 模型。 |
| `novita` | `[providers.novita]` | `NOVITA_API_KEY` | `NOVITA_BASE_URL`；默认 `https://api.novita.ai/openai/v1` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash` | 面向 DeepSeek 模型 ID 的 OpenAI 兼容托管路由。模型覆盖使用配置或 `CODEWHALE_MODEL` / `DEEPSEEK_MODEL`。 |
| `fireworks` | `[providers.fireworks]` | `FIREWORKS_API_KEY` | `FIREWORKS_BASE_URL`；默认 `https://api.fireworks.ai/inference/v1` | `accounts/fireworks/models/deepseek-v4-pro` | OpenAI 兼容托管路由。模型覆盖使用配置或 `CODEWHALE_MODEL` / `DEEPSEEK_MODEL`。 |
| `siliconflow` | `[providers.siliconflow]` | `SILICONFLOW_API_KEY` | `SILICONFLOW_BASE_URL`；默认 `https://api.siliconflow.com/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | OpenAI 兼容托管路由。官方文档使用 `.com` 端点。接受 `SILICONFLOW_MODEL`。推理别名 `deepseek-reasoner` 和 `deepseek-r1` 映射到 Pro；`deepseek-chat` 和 `deepseek-v3` 映射到 Flash。 |
| `siliconflow-CN` | `[providers.siliconflow_cn]` | `SILICONFLOW_API_KEY` | `SILICONFLOW_BASE_URL`；默认 `https://api.siliconflow.cn/v1` | 使用 SiliconFlow 模型集 | 中国区 SiliconFlow 路由。未设置时回退到 `[providers.siliconflow]` 的 api_key / base_url / model。用 `provider = "siliconflow-CN"` 或 `CODEWHALE_PROVIDER=siliconflow-CN` 选择它。 |
| `arcee` | `[providers.arcee]` | `ARCEE_API_KEY` | `ARCEE_BASE_URL`；默认 `https://api.arcee.ai/api/v1` | `trinity-large-thinking`, `trinity-large-preview` | Arcee AI 直连 OpenAI 兼容路由，按 256K 上下文 BF16 服务跟踪。接受 `ARCEE_MODEL`。OpenRouter 的 `arcee-ai/trinity-large-thinking` 仍是 OpenRouter 命名空间的模型 ID；直连 Arcee 使用裸 `trinity-large-thinking` ID。 |
| `moonshot` | `[providers.moonshot]` | `MOONSHOT_API_KEY`, `KIMI_API_KEY` | `MOONSHOT_BASE_URL`, `KIMI_BASE_URL`；默认 `https://api.moonshot.ai/v1` | 直连 Moonshot：`kimi-k3`, `kimi-k2.7-code`, `kimi-k2.7-code-highspeed`, `kimi-k2.6`；Kimi Code 会员：`k3`, `kimi-for-coding`, `kimi-for-coding-highspeed`（在 `https://api.kimi.com/coding/v1`） | Moonshot/Kimi 路由。精确的直连 `kimi-k3` 路由使用文档中记载的 Formula 网页搜索工具/fiber 循环；直连的 `kimi-k2.6` 保留内置的 `$web_search` 约定，精确的 Kimi Code 会员路由使用其结构化的 `/search` 服务。相邻路径、K2.7 直连模型以及跨产品的模型 ID 不会继承原生搜索。`kimi` 和 `kimi-k2` 别名选择 `kimi-k2.7-code`；接受 `MOONSHOT_MODEL`, `KIMI_MODEL_NAME` 和 `KIMI_MODEL`。Kimi 思考通过 `reasoning_content` 流式返回；Codewhale 把它保留在 Thinking 单元格中，并为思考/工具调用连续性重放它。直连 K3 请使用精确的 `base_url = "https://api.moonshot.ai/v1"` 和 `model = "kimi-k3"`；它始终思考，接收顶层 `reasoning_effort = "low" \| "high" \| "max"`（`off` 规范化为 `low`），只使用 `max_completion_tokens`，并按 [K3 快速入门](https://platform.kimi.ai/docs/guide/kimi-k3-quickstart)省略 `temperature`/`top_p`。Kimi Code K3 请使用来自 [Kimi Code 控制台](https://www.kimi.com/code/console)的密钥、精确的 `base_url = "https://api.kimi.com/coding/v1"` 和裸 `model = "k3"`；`off` 变成启用的 `low`，而正常分派的 `auto` 选择并发送具体的 Codewhale 层级。只有省略推理设置时才由提供商默认值控制。该会员路由默认安全地使用 262,144 上下文 token；[Kimi Code 模型层级表](https://www.kimi.com/code/docs/en/kimi-code/models.html)授予 Allegretto 及以上套餐最高 1M，这些套餐可将其表示为 `context_window = 1048576`。`k3[1m]` 仅限 Claude Code，Codewhale 拒绝它。`kimi-for-coding` 仍是有效的 K2.7 会员路由，`kimi-for-coding-highspeed` 是它自己的高速名册条目（262,144 上下文）；会员 id 在直连平台端点上被拒绝，`kimi-k3` 在会员端点上仍被拒绝。计费由路由解析到的端点决定，对照两个精确的产品端点各判断一次：直连 Moonshot（`https://api.moonshot.ai/v1` 或默认值）按用量计费并显示美元估算，精确的 Kimi Code 会员端点按 Kimi Code 额度计费且从不显示美元估算，其他任何情况——网关主机、邻近的 Kimi 托管路径——报告 `cost: unknown` 而不是借用任一产品。导入的没有 `base_url` 的 Kimi Code token 仍解析到会员端点，因此按 Kimi Code 额度计费，绝不累积美元。完成的回合（父智能体或子智能体）从构建其客户端时不可变的端点回执计费，绝不从之后重新读取的配置计费：`MOONSHOT_BASE_URL`/`KIMI_BASE_URL` 只合并到*活动*提供商的表中，回合内的提供商切换可以把常驻配置移离实际运行的路由。遗留的 `auth_mode = "kimi_oauth"` 在不探测 Kimi CLI 文件的情况下失败于 API 密钥指引。Codewhale 不冒充 `kimi_cli` 或 `kimi_code_cli`。**中国区密钥：** 贡献者现场证据（@vFONGv，PR #5229，Windows 10 上验证）报告中国区 Moonshot 密钥必须搭配 `base_url = "https://api.moonshot.cn/v1"`；留在默认国际主机（`https://api.moonshot.ai/v1`）上会认证失败。我们自己没有中国区密钥来验证这一点，因此记录为用户报告而非测试过的路由。另请注意，仅编辑 `base_url` 不会生效，直到为该提供商重新运行 `codewhale auth set`。 |
| `google` | `[providers.google]` | `GOOGLE_API_KEY`, `GEMINI_API_KEY` | `GOOGLE_BASE_URL`, `GEMINI_BASE_URL`；默认 `https://generativelanguage.googleapis.com/v1beta/openai/` | `gemini-3.1-pro-preview`（默认）；`/model` 还列出 `gemini-3-pro-preview`, `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-2.5-pro`, `gemini-2.5-flash` | Google Gemini 作为官方 OpenAI 兼容 Chat Completions 路由上的自有后端。思考模型在工具调用时捕获 `extra_content.google.thought_signature` 并在助手工具调用消息中重放它；重放一个未捕获签名的工具调用会失败关闭并给出可操作的错误，而不是让工具循环中断。`gemini-2.5-flash-lite` 默认关闭思考，并以警告降级。推理强度映射到文档化的 `google.thinking_config.thinking_level`（`low`/`high`）。该方言绑定精确的官方 base URL：指向其他网关的 `google` 行获得普通 OpenAI 语义且无签名要求。Codewhale 从不读取 Google OAuth 文件；只使用 AI Studio API 密钥。本环境未对真实端点做实时测试。 |
| `zai` | `[providers.zai]` | `ZAI_API_KEY`, `Z_AI_API_KEY`, `ZHIPU_API_KEY`, `GLM_API_KEY` | `ZAI_BASE_URL`, `Z_AI_BASE_URL`；默认 `https://api.z.ai/api/coding/paas/v4`；通用 API `https://api.z.ai/api/paas/v4` 和 `https://open.bigmodel.cn/api/paas/v4` | `GLM-5.3` 默认；`/model` 还列出 `GLM-5.3-Flash`, `GLM-5.2`, `GLM-5.1` 和 `GLM-5-Turbo` | Z.AI GLM Coding Plan 路由。三条第一方 Chat 路由（api.z.ai 的两个产品与 BigModel 通用平台端点）共用同一种请求方言，因此推理控制——GLM-5.2 的思考开关、分档思考强度，以及把 `off` 改写为 `enabled` + `reasoning_effort: "low"` 的 GLM-5.3 强制思考改写——在 BigModel 上同样生效；`/preview` 等相邻路径仍保持失败关闭。两个通用 API 产品提供结构化的提供商原生网页搜索（全球为 `search-prime`，中国区为 `search_std`）；Coding Plan 和兼容的自定义端点不会继承它。`GLM-5.3` 是默认值，也是一等选择器行（`model = "GLM-5.3"` 或 `ZAI_MODEL=GLM-5.3`）；`GLM-5.3-Flash` 是 1M 多模态的快速同系模型（`model = "GLM-5.3-Flash"`）。显式选择 `GLM-5.2` 会保留它自己的 id。在 Z.ai 发布不同的 5.3 元数据之前，5.3 的限制和推理选项继承自 `GLM-5.2`；5.3 不带价格。Flash 采用已公布的 $0.15/$0.50 价目。在未开通 5.3 的账户上，真实调用仍可能以权限代码 1311 返回 429。 |
| `stepfun` | `[providers.stepfun]` | `STEPFUN_API_KEY`, `STEP_API_KEY` | `STEPFUN_BASE_URL`, `STEP_BASE_URL`；默认 `https://api.stepfun.ai/v1`；Coding Plan 端点 `https://api.stepfun.ai/step_plan/v1` | `step-3.7-flash` | StepFun / StepFlash 直连 OpenAI 兼容路由。`/provider` 设置询问密钥属于哪种计费路由——按量付费还是 Step Plan——对照所选端点验证密钥，并把答案只写入 `[providers.stepfun].base_url`。既非两条已识别路由的 base URL 会被保留，问题被跳过。你也可以手动把 `[providers.stepfun].base_url` 或 `STEP_BASE_URL` 设为 Coding Plan URL。离线核算把已识别路由标记为 `stepfun-payg` 或 `stepfun-plan`，不持久化原始端点，只有标准 PAYG 路由接收 token 定价。接受 `STEPFUN_MODEL` 和 `STEP_MODEL`。 |
| `minimax` | `[providers.minimax]` | `MINIMAX_API_KEY` | `MINIMAX_BASE_URL`；默认 `https://api.minimax.io/v1`；中国 `https://api.minimaxi.com/v1` | `MiniMax-M3`, `MiniMax-M2.7`, `MiniMax-M2.7-highspeed`, `MiniMax-M2.5`, `MiniMax-M2.5-highspeed`, `MiniMax-M2.1`, `MiniMax-M2.1-highspeed`, `MiniMax-M2` | MiniMax 直连 OpenAI 兼容路由。Codewhale 发送 `reasoning_split = true`，这样 MiniMax 思考与答案文本分开到达。两种 MiniMax 方言都在相同端点和相同密钥上售卖按量付费和 Token Plan，因此计费从凭据*产品*分类，绝不由端点或默认值决定。`[providers.minimax]`/`[providers.minimax_anthropic]` 中的 `mode = "token-plan"`，或形如 `sk-cp…` 的 Token Plan 密钥，按 MiniMax Token Plan 额度计费且无美元估算；显式的按量付费模式（`pay-as-you-go`/`payg`/`metered`）胜过密钥形状。密钥的产品前缀只有在密钥位于配置中、由 `api_key_env` 绑定或作为 `MINIMAX_API_KEY` 导出到官方端点时才可见——通过 `codewhale auth set` 保存的密钥（secret store / OS keyring）刻意不被读取用于计费分类。没有显式模式且没有可见的产品标记时，路由报告 `cost: unknown` 而不是假定按量付费，因此 Token Plan 账户永远不会被收取虚构的美元。自定义/网关端点也以 `cost: unknown` 失败关闭。官方 M3 输入模态是文本、图像和视频；M2.7 仅文本。 |
| `minimax-anthropic` | `[providers.minimax_anthropic]` | `MINIMAX_API_KEY` | `MINIMAX_ANTHROPIC_BASE_URL`；默认 `https://api.minimax.io/anthropic`；中国 `https://api.minimaxi.com/anthropic` | `MiniMax-M3`, `MiniMax-M2.7` | MiniMax 直连 Anthropic 兼容 Messages 路由。保留 `/anthropic` 后缀，因为 Codewhale 会追加 `/v1/messages`；该路由使用 `x-api-key`。M3 支持自适应或禁用思考。M2.7 始终启用思考。 |
| `sglang` | `[providers.sglang]` | 可选 `SGLANG_API_KEY` | `SGLANG_BASE_URL`；默认 `http://localhost:30000/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | 自托管 OpenAI 兼容路由。本地部署通常省略认证。接受 `SGLANG_MODEL`。 |
| `vllm` | `[providers.vllm]` | 可选 `VLLM_API_KEY` | `VLLM_BASE_URL`；默认 `http://localhost:8000/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | 自托管 vLLM OpenAI 兼容路由。本地部署通常省略认证。接受 `VLLM_MODEL`。 |
| `ollama` | `[providers.ollama]` | 本地可选 `OLLAMA_API_KEY` | `OLLAMA_BASE_URL`；默认 `http://localhost:11434/v1` | 本地目录的实况标签；刷新前占位 `unknown`；提供商提示的自定义标签直通 | 本地 Ollama 默认无密钥。接受 `OLLAMA_MODEL`。标题栏不得绘制本机守护进程未列出的托管 id。 |
| `ollama-cloud` | `[providers.ollama_cloud]` | `OLLAMA_CLOUD_API_KEY`, 然后是 `OLLAMA_API_KEY` | `OLLAMA_CLOUD_BASE_URL`；默认 `https://ollama.com/v1` | `gpt-oss:120b`；任意的提供商自有 ID 直通 | 托管 OpenAI 兼容 `/v1/chat/completions` 路由。在 `ollama-cloud` 下保存凭据；精确的已发布 `ollama` + Cloud URL 元组与其遗留表和秘密槽位具有有界的只读内存兼容性。接受 `OLLAMA_CLOUD_MODEL`。 |
| `huggingface` | `[providers.huggingface]` | `HUGGINGFACE_API_KEY`, `HF_TOKEN` | `HUGGINGFACE_BASE_URL`, `HF_BASE_URL`；默认 `https://router.huggingface.co/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | Hugging Face Inference Providers OpenAI 兼容路由器路由。接受的别名：`huggingface`, `hugging-face`, `hugging_face`, `hf`。Org 前缀的模型 ID 直通。接受 `HUGGINGFACE_MODEL` 和 `HF_MODEL`。Hub 浏览/导出是未来的独立功能。 |
| `modelscope` | `[providers.modelscope]` | `MODELSCOPE_API_KEY` | `MODELSCOPE_BASE_URL`；默认 `https://api-inference.modelscope.cn/v1` | `Qwen/Qwen3.5-397B-A17B`（默认）, `Qwen/Qwen3.5-122B-A10B`, `Qwen/Qwen3.5-27B`, `Qwen/Qwen3.5-35B-A3B`, `Qwen/Qwen3.8-27B`, `Qwen/Qwen3.8-Flash-Next`, `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Pro-0813`, `deepseek-ai/DeepSeek-V4.1-Flash`, `ZhipuAI/GLM-4.7-Flash`, `ZhipuAI/GLM-5.2` | 阿里魔搭社区 OpenAI 兼容推理 API 路由。模型 ID 直通。接受 `MODELSCOPE_MODEL`。 |
| `deepinfra` | `[providers.deepinfra]` | `DEEPINFRA_API_KEY`, `DEEPINFRA_TOKEN` | `DEEPINFRA_BASE_URL`；默认 `https://api.deepinfra.com/v1/openai` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | DeepInfra OpenAI 兼容路由。OpenAI SDK 的直接替代品。 |
| `together` | `[providers.together]` | `TOGETHER_API_KEY` | `TOGETHER_BASE_URL`；默认 `https://api.together.xyz/v1` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash`, `thinkingmachines/inkling` | Together AI OpenAI 兼容路由。接受 `TOGETHER_MODEL`。模型别名 `deepseek-v4-pro` 和 `deepseek-v4-flash` 规范化为 Together 的 org 前缀 ID；`inkling` 和 `together-inkling` 规范化为 Together 发布的小写 Inkling 线协议 ID。Inkling 使用 Thinking Machines 的[官方模型仓库](https://huggingface.co/thinkingmachines/Inkling)中精确的 `none`/`minimal`/`low`/`medium`/`high`/`max` 推理词汇。Together 的[发布帖](https://www.together.ai/blog/together-ai-brings-thinking-machines-labs-new-model-inkling-on-day-0)目前称 Inkling 以 1M 上下文上线，而其[模型详情页](https://www.together.ai/models/inkling)称即将推出 256K 上下文且不公布价格。在 Together 活跃的 `/models` 端点和 Models.dev 目录解决该冲突之前，Inkling 不会种入 Codewhale 的离线选择器，也不会推断路由特定的上下文或成本。 |
| `qianfan` | `[providers.qianfan]` | `QIANFAN_API_KEY`, `BAIDU_QIANFAN_API_KEY` | `QIANFAN_BASE_URL`, `BAIDU_QIANFAN_BASE_URL`；默认 `https://api.baiduqianfan.ai/v1` | `ernie-4.0-turbo-8k`；提供商范围的定制 Qianfan 服务/模型 ID 直通 | 百度千帆 OpenAI 兼容路由。请求使用 Bearer 认证和 Chat Completions 负载。接受 `QIANFAN_MODEL` 和 `BAIDU_QIANFAN_MODEL`；别名 `baidu-qianfan`, `baidu_qianfan` 和 `baidu` 解析到该提供商。千帆文档中工具/函数调用按模型范围限定，因此 Codewhale 保留所选线协议模型，把实时能力证明留给后续的路由/能力工作。 |
| `openai-codex` | `[providers.openai_codex]` | 官方“使用 ChatGPT 登录”（`codewhale auth chatgpt` / `/provider setup openai-codex`） | 官方端点 `https://api.openai.com/v1` | 当前账户目录；手动配置的模型 ID 仍属于显式选择 | **实验性。** 使用公共 Responses 端点（`/v1/responses`），要求已验证的 `chatgpt.tokens.use.direct` 授权。开源应用动态注册会为所选账户/工作区签发客户端 ID，Codewhale 保护并续期自己的令牌。可选模型由账户目录提供；公开模型表和旧版 Codex 令牌不能证明套餐使用权限。不会自动切换计费路径。设置、额度和预览限制见[使用 ChatGPT 登录](#使用-chatgpt-登录)。 |
| `anthropic` | `[providers.anthropic]` | `ANTHROPIC_API_KEY` | `ANTHROPIC_BASE_URL`；默认 `https://api.anthropic.com` | `claude-opus-4-8`, `claude-sonnet-4-6`（默认）, `claude-haiku-4-5` | 原生 Anthropic Messages API 路由（`/v1/messages`、`x-api-key` + `anthropic-version: 2023-06-01`）——不是 OpenAI 兼容。通过 `cache_control` 断点提示缓存、自适应思考 + `output_config.effort`、按原样重放带签名的思考块、按 #2961 规范化的缓存遥测。接受 `ANTHROPIC_MODEL`。 |
| `openmodel` | `[providers.openmodel]` | `OPENMODEL_API_KEY` | `OPENMODEL_BASE_URL`；默认 `https://api.openmodel.ai` | `deepseek-v4-flash`；提供商范围的定制模型 ID 直通 | OpenModel Anthropic 兼容 Messages 路由。使用 `/v1/messages`、Bearer 认证和 `anthropic-version: 2023-06-01`；OpenModel 按模型 id 选择 DeepSeek、DashScope、Xiaomi、Claude 和其他路由。接受 `OPENMODEL_MODEL`。 |
| `sakana` | `[providers.sakana]` | `FUGU_API_KEY`, `SAKANA_API_KEY` | `SAKANA_BASE_URL`；默认 `https://api.sakana.ai/v1` | `fugu`（默认）, `fugu-ultra-20260615` | Sakana AI Fugu OpenAI 兼容路由。标准 Chat Completions 线协议；支持流式。`fugu-ultra-20260615` 是重型/推理变体。环境变量别名：`FUGU_API_KEY`（主）, `SAKANA_API_KEY`；提供商别名：`sakana-ai`, `sakana_ai`, `fugu`。 |
| `longcat` | `[providers.longcat]` | `LONGCAT_API_KEY` | `LONGCAT_BASE_URL`；默认 `https://api.longcat.chat/openai/v1` | `LongCat-2.0`（默认） | 美团 LongCat 精选模型网关。OpenAI 兼容 Chat Completions 线协议。在 https://longcat.chat/platform 注册获取 API 密钥。提供商别名：`long-cat`, `meituan-longcat`, `meituan`。 |
| `opencode-go` | `[providers.opencode_go]` | `OPENCODE_GO_API_KEY` | `OPENCODE_GO_BASE_URL`；默认 `https://opencode.ai/zen/go/v1` | `deepseek-v4-pro`（默认）, `grok-4.5`, `glm-5.2`, `glm-5.1`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `deepseek-v4-flash`, `mimo-v2.5`, `mimo-v2.5-pro` | [OpenCode Go](https://opencode.ai/docs/go/) 订阅路由，使用 OpenAI 兼容 Chat Completions。接受 `OPENCODE_GO_MODEL`。Codewhale 使用裸线协议 ID；常见的 `opencode-go/<model-id>` 输入别名规范化为裸 ID。只在 Anthropic `/messages` 端点文档化的 Go 模型，在 Codewhale 支持按模型选择线协议之前被该路由刻意不公布。计费界面显示 Go 额度而不是 token 价格估算。 |
| `opencode-zen` | `[providers.opencode_zen]` | `OPENCODE_ZEN_API_KEY`, 回退 `OPENCODE_API_KEY` | `OPENCODE_ZEN_BASE_URL`；默认 `https://opencode.ai/zen/v1` | `gpt-5.6`（默认）；当前的 GPT、Claude、Qwen、DeepSeek、MiniMax、GLM、Kimi、Grok、Muse Spark 和免费模型 ID | [OpenCode Zen](https://opencode.ai/docs/zen/) 模型感知网关。接受 `OPENCODE_ZEN_MODEL`，官方 `opencode/<model-id>` 选择器规范化为裸线协议 ID。每个模型的线协议先取自精选快照，再取自其 [Models.dev](https://models.dev) 行声明的 AI SDK 包：GPT、Grok 和 Muse Spark 行使用 `/responses`；Claude 和大多数 Qwen 行使用 `/messages`；DeepSeek、MiniMax、GLM、Kimi、`qwen3.8-max` 和免费行使用 `/chat/completions`。Responses 和 Chat Completions 用 Bearer `Authorization` 认证，而 Anthropic Messages 用 `x-api-key`；这些路由都不用 ChatGPT/Codex OAuth 指引或头。Gemini 失败关闭，因为其模型特定的 Google 线协议未实现；Models.dev 标记为 `deprecated` 的行以及任何已加载目录都未列出的模型也失败关闭。见 [OpenCode Zen 协议目录](#opencode-zen-协议目录)。 |
| `meta` | `[providers.meta]` | `META_MODEL_API_KEY`, `MODEL_API_KEY` | `META_MODEL_API_BASE_URL`, `MODEL_API_BASE_URL`；默认 `https://api.meta.ai/v1` | `muse-spark-1.2`（默认） | [Meta Model API](https://developer.meta.com/ai/resources/blog/build-with-muse-spark/) 公开预览路由，使用 OpenAI 兼容 Chat Completions。Muse Spark 1.2 保留其线协议 ID、工具支持、1M 上下文、32K 输出元数据和 `none` 到 `xhigh` 的推理强度。接受 `META_MODEL_API_MODEL` 和 `MODEL_API_MODEL`。提供商别名：`meta-ai`, `meta_model_api`, `muse`, `muse-spark`。 |
| `telecomjs` | `[providers.telecomjs]` | `TELECOMJS_API_KEY` | `TELECOMJS_BASE_URL`；默认 `https://aigw.telecomjs.com/v1` | 保守回退 `deepseek-v4-pro`；配置密钥后认证的 `/models` 行 | TelecomJS TokenHub OpenAI 兼容 Chat Completions 路由。实时目录按提供商和密钥指纹隔离，过期的行在瞬时刷新失败时存活，不支持的推理请求字段被省略。接受 `TELECOMJS_MODEL`。提供商别名：`telecom-js`, `telecom_js`, `telecomjs-cn`, `tokenhub`。 |
| `mistral` | `[providers.mistral]` | `MISTRAL_API_KEY` | `MISTRAL_BASE_URL`；默认 `https://api.mistral.ai/v1` | `mistral-code-latest`（默认；`codestral-latest` 接受为别名）, `mistral-medium-latest`（别名：`mistral-medium-3-5`）, `mistral-small-latest`（别名：`mistral-small-2603`）, `mistral-large-latest` | Mistral AI（la Plateforme）OpenAI 兼容 Chat 路由。在文档化的第一方 HTTPS `/v1` 主机上，Medium 和 Small 发送可调的 `reasoning_effort`（仅 `none` 或 `high`），解析 Mistral 的多态 thinking/text 块，并以相同线协议形状重放存储的思考。已废弃的原生 Magistral ID 仍是显式配置兼容路由：它们始终思考，从不接收可调 effort 字段。Code 和 Large 不推理。除非是文档化的第一方主机之一，自定义 `MISTRAL_BASE_URL` 保持通用 Chat 语义。接受 `MISTRAL_MODEL`。提供商别名：`mistral-ai`, `mistralai`, `la-plateforme`。 |
| `edenai` | `[providers.edenai]` | `EDENAI_API_KEY` | `EDENAI_BASE_URL`；默认 `https://api.edenai.run/v3`；欧盟 `https://api.eu.edenai.run/v3` | `deepseek/deepseek-v4-pro`（默认）；`provider/model` id 的实时 `/models` 目录 | Eden AI OpenAI 兼容聚合网关。目录行保持提供商范围；通用推理控制被省略，因为受支持的字段取决于所选上游家族。接受 `EDENAI_MODEL`。默认的 `deepseek/deepseek-v4-pro` 只列在全球目录上；在欧盟端点上把 `EDENAI_MODEL`（或 `model`）设为欧盟 `/models` 列表中的一行，例如 `qwen/deepseek-v4-pro`。提供商别名：`eden-ai`, `eden_ai`。 |
| `zenmux` | `[providers.zenmux]` | `ZENMUX_API_KEY` | `ZENMUX_BASE_URL`；默认 `https://zenmux.ai/api/v1` | `deepseek/deepseek-v4.1-flash`（默认）；`provider/model` id 的实时 `/models` 目录（免密钥可读） | ZenMux OpenAI 兼容聚合网关（约 190 个模型）。目录行保持提供商范围；通用推理控制被省略，因为受支持的字段取决于所选上游家族。接受 `ZENMUX_MODEL`。提供商别名：`zen-mux`, `zen_mux`。 |
| `csdn` | `[providers.csdn]` | `CSDN_API_KEY` | `CSDN_BASE_URL`；默认 `https://ai.csdn.net/api/model/v1` | `glm_for_coding`（默认；Coding Plan 专用模型 id）；其他市场模型 id 直通 | CSDN 星图（Starmap）OpenAI 兼容托管平台。Coding Plan 密钥与通用市场密钥共用一个端点，因此计费取决于凭据产品而非 URL 本身：路由 `glm_for_coding`（出厂默认）或设置 `mode = "coding_plan"`/`"plan"`/`"subscription"` 按 CSDN Coding Plan 配额计费，不显示美元估算；任何其他模型或显式 `pay-as-you-go`/`metered` 模式按量计费；无法识别的模式或 `ai.csdn.net/api/model/v1` 之外的端点报告 `cost: unknown`。接受 `CSDN_MODEL`。提供商别名：`csdn-ai`, `csdn_ai`, `csdn-coding-plan`, `csdn_coding_plan`, `starmap`。 |
| `concentrate` | `[providers.concentrate]` | `CONCENTRATE_API_KEY` | `CONCENTRATE_BASE_URL`；默认 `https://api.concentrate.ai/v1` | `deepseek-v4-pro`（默认；使用普通目录 ID 时由网关选择上游提供商）；形如 `openai/gpt-5.6-sol` 的 `provider/model` ID 会固定一个上游；`concentrate/auto` 是网关自己的路由器；无需认证即可读取的实时 `/v1/models` 目录 | Concentrate OpenAI Responses 兼容网关（`POST /v1/responses`，Bearer 认证，使用 Universal API 密钥）。需主动启用，且仅支持自带密钥（BYOK）：用你自己的密钥、走你自己的 Concentrate 账单，Codewhale 不收任何费用，也没有托管的默认路由。请求只携带文档中记载的字段（系统提示词作为 `system` 输入项传递）。约定见：[API 简介](https://concentrate.ai/docs/api-reference/introduction)、[请求参数](https://concentrate.ai/docs/api-reference/endpoint/request-parameters)、[流式传输](https://concentrate.ai/docs/api-reference/endpoint/streaming)、[错误](https://concentrate.ai/docs/api-reference/endpoint/errors)。参见 [Concentrate 说明](#concentrate-说明)。 |
| `codewhale` | `[providers.codewhale]` | `CODEWHALE_API_KEY` | `CODEWHALE_API_BASE`；默认 `https://api.codewhale.net/v1` | `deepseek/deepseek-v4-pro`（默认）、`anthropic/claude-sonnet-5`、`openai/gpt-5.6` 仅是离线引导行；目录以已认证的 `GET /v1/models` 返回的、账户已连接的提供商列表为准 | Codewhale API：基于客户已连接到其 Codewhale 账户的提供商密钥，提供账户级的模型访问。一个带 `models:infer` 权限范围的 `cwc_key_…` 账户 API 密钥即可认证所有模型。模型 ID 就是账户目录返回的 `provider/model` 原样字符串，每一行都会声明自己的协议（`chat-completions` → `/v1/chat/completions`，`anthropic-messages` → `/v1/messages`，`responses` → `/v1/responses`）；所有协议都使用 `Authorization: Bearer`，绝不使用 `x-api-key`。`CODEWHALE_API_BASE` 必须是 HTTPS，回环地址除外。用 `codewhale account keys set <provider>` 连接提供商密钥。提供商别名：`codewhale-api`, `cw-api`, `codewhale-cloud`。 |
| `xai` | `[providers.xai]` | `XAI_API_KEY`、Codewhale 自有的设备 OAuth，或显式的只读 Grok CLI 授权 | `XAI_BASE_URL`；默认 `https://api.x.ai/v1` | `grok-4.6`（默认）, `grok-4.7`, `grok-4.5`, `grok-4.3`, `grok-build`, `grok-composer-2.5-fast`, `grok-4.20-0309-reasoning`, `grok-4.20-0309-non-reasoning` | xAI/Grok OpenAI 兼容 Chat Completions 路由。Grok 4.6 有 500K 上下文窗口、文本/图像输入、函数调用、结构化输出、服务端 web 搜索和 `low`/`medium`/`high`/`xhigh` 推理（默认 `high`）。[Grok 4.7](https://docs.x.ai/docs/models/grok-4.7) 有相同的 500K 窗口、文本/图像输入、函数调用、结构化输出、推理阶梯和 $2.00 / $0.50 缓存 / $6.00 费率；xAI 尚未记录其 web 搜索支持，因此 Codewhale 不声明。Grok 推理无法关闭（[推理指南](https://docs.x.ai/docs/guides/reasoning)）。提示达到 200K token 时其标准费率翻倍；同样的 2 倍长上下文规则适用于 `grok-4.5`（500K 上下文，$2.00 / $0.30 缓存 / $6.00）和 `grok-4.3`（1M 上下文，$1.25 / $0.20 缓存 / $2.50），按其[模型页](https://docs.x.ai/docs/models/grok-4.5)。没有文档化的 `latest`/`fast` 别名，也没有公布的数字输出上限。**API 密钥**（默认）：来自 console.x.ai 的 Bearer token，通过 `XAI_API_KEY` / keyring / `api_key`。**OAuth**：`codewhale auth xai-device` 使用 SSH 友好的设备登录和 Codewhale 自有的存储，它可能自行刷新。现有的 Grok CLI 凭据需要 `codewhale auth external-consent --provider xai --mode read-only`；被授予的外部文件从不刷新或重写。OAuth 在某些 SuperGrok 层级上可能返回 HTTP 403——把 API 密钥作为可靠回退。接受 `XAI_MODEL`。提供商别名：`x-ai`, `x_ai`, `grok`。 |
| `modelstudio-token-plan` | `[providers.modelstudio_token_plan]` | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` | `MODELSTUDIO_TOKEN_PLAN_BASE_URL`；默认 `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | `qwen3.8-max`（默认）, `qwen3.8-max-preview`, `qwen3.7-plus`, `qwen3.7-max`, `qwen3.6-flash`, `deepseek-v4-pro`, `deepseek-v4-flash-0731`, `glm-5.2` | 阿里云 Model Studio Token Plan OpenAI 兼容 Chat Completions 路由。Token Plan 个人版和团队版共享该端点。`qwen3.8-max`、`qwen3.7-plus` 和 `qwen3.7-max` 可以通过 Token Plan Responses Harness 使用提供商原生网页搜索；预览版、Coding Plan 和 Anthropic 路由不会继承该能力。列出的所有模型都是可推理的文本/编码模型。DeepSeek 和 GLM 条目是提供商范围的，不与第一方路由冲突。接受 `MODELSTUDIO_TOKEN_PLAN_MODEL`。提供商别名：`modelstudio-token-plan`, `alibaba-token-plan`, `dashscope-token-plan`。 |
| `modelstudio-token-plan-anthropic` | `[providers.modelstudio_token_plan_anthropic]` | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` | 默认 `https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic` | 与 `modelstudio-token-plan` 相同的模型目录 | Token Plan Anthropic 兼容 Messages 路由（`/apps/anthropic`）。与 OpenAI 方言相同的 API 密钥。提供商别名：`modelstudio-token-plan-anthropic`, `alibaba-token-plan-anthropic`。 |
| `modelstudio-coding-plan` | `[providers.modelstudio_coding_plan]` | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` | `MODELSTUDIO_CODING_PLAN_BASE_URL`；默认 `https://coding-intl.dashscope.aliyuncs.com/v1` | `qwen3.8-max`（默认）；与 Token Plan 相同的目录 | 阿里云 Model Studio Coding Plan OpenAI 兼容 Chat Completions 路由。接受 `MODELSTUDIO_CODING_PLAN_MODEL`。提供商别名：`modelstudio-coding-plan`, `alibaba-coding-plan`, `dashscope-coding-plan`。 |
| `modelstudio-coding-plan-anthropic` | `[providers.modelstudio_coding_plan_anthropic]` | `MODELSTUDIO_API_KEY`, `DASHSCOPE_API_KEY` | 默认 `https://coding-intl.dashscope.aliyuncs.com/apps/anthropic` | 与 `modelstudio-coding-plan` 相同的模型目录 | Coding Plan Anthropic 兼容 Messages 路由（`/apps/anthropic`）。提供商别名：`modelstudio-coding-plan-anthropic`, `alibaba-coding-plan-anthropic`。 |

StepFun 的四个编码模型既可通过其标准 API 使用，也可通过 [Step Plan](https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api) 使用。请先在 `/provider` 中选择计费路由，再在 `/model` 中选择模型；已有的 Step 3.7 选择保持不变。[Step 5 Preview](https://platform.stepfun.ai/docs/en/guides/models/step-5-preview) 拥有 1M 上下文窗口。Step 5 和 Step 3.7 支持 low/medium/high 三档推理；Step 3.5 Flash 2603 支持 low/high；基础的 Step 3.5 使用提供商的默认推理。[公布的 API 价格](https://platform.stepfun.ai/docs/en/guides/pricing/details)只适用于已验证的按量付费（PAYG）路由；Step Plan 显示的是订阅额度。语音、音乐和图像生成模型使用单独的接口，不会作为编码模型呈现。提供商的视频能力元数据并不意味着每个 Codewhale 客户端都能附加视频。

### OpenCode Zen 协议目录

Zen Responses 和 Chat Completions 请求使用 Bearer `Authorization` 认证；Zen Anthropic Messages 请求使用 `x-api-key`。这些路由都不会添加 ChatGPT/Codex OAuth 头。

每个 Zen 模型的线协议来自两个来源，都不会按模型 ID 家族猜测（`qwen3.8-flash` 走 Messages，而 `qwen3.8-max` 走 Chat）：

1. **精选快照**：编译进 Codewhale，已对照[官方端点表](https://opencode.ai/docs/zen/)和 [Models.dev](https://models.dev) 的 `opencode` provider 核实。它是离线下限；当目录行为同一 ID 声明了不同线协议时，以它为准。
2. **Models.dev 目录**：其 `opencode` provider 就是 Zen 公布的目录，每个模型行都写明 OpenCode 自己使用的 AI SDK 包。Codewhale 精确映射：`@ai-sdk/openai` → Responses，`@ai-sdk/anthropic` → Anthropic Messages，`@ai-sdk/openai-compatible`（provider 默认值）→ Chat Completions。本版本之后发布的 Zen 模型只要出现在目录中即可路由，无需发布新版 Codewhale。交互式 TUI 和 `codewhale exec` 都会加载持久化的目录；用 `codewhale models --update` 刷新。

精选快照：

- Responses：`gpt-6-astra`, `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-sol`,
  `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.5-pro`, `gpt-5.4`,
  `gpt-5.4-pro`, `gpt-5.4-mini`, `gpt-5.4-nano`, `gpt-5.3-codex`,
  `gpt-5.3-codex-spark`, `gpt-5.2`, `gpt-5.2-codex`, `gpt-5.1`,
  `gpt-5.1-codex`, `gpt-5.1-codex-max`, `gpt-5.1-codex-mini`, `gpt-5`,
  `gpt-5-codex`, `gpt-5-nano`, `grok-4.7`, `grok-4.6`, `grok-4.5`,
  `grok-build-0.1`, `muse-spark-1.3`, `muse-spark-1.3-contributor-free`,
  `muse-spark-1.2`, `muse-spark-1.2-contributor`,
  `muse-spark-1.2-contributor-free`。
- Anthropic Messages：`claude-fable-5-1`, `claude-fable-5`,
  `claude-opus-5-5`, `claude-opus-5`, `claude-opus-4-8`, `claude-opus-4-7`,
  `claude-opus-4-6`, `claude-opus-4-5`, `claude-sonnet-5-5`,
  `claude-sonnet-5`, `claude-sonnet-4-6`, `claude-sonnet-4-5`,
  `claude-sonnet-4`, `claude-haiku-4-5`, `qwen3.8-flash`, `qwen3.7-max`,
  `qwen3.7-plus`, `qwen3.6-plus`, `qwen3.5-plus`。
- Chat Completions：`deepseek-v4.1-flash`, `deepseek-v4-pro`,
  `deepseek-v4-flash`, `deepseek-v4-flash-vision-exp`, `minimax-m3`,
  `minimax-m2.7`, `minimax-m2.5`, `glm-5.3-flash`, `glm-5.3`, `glm-5.2`,
  `glm-5.1`, `glm-5`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `kimi-k2.5`,
  `qwen3.8-max`, `big-pickle`, `space-bunny-free`,
  `longcat-2.5-preview-free`, `mimo-v2.6-flash-free`, `mimo-v2.5-free`,
  `ling-3.0-flash-fin-free`, `north-mini-code-free`,
  `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free`,
  `deepseek-v4-flash-free`。

以下情况在本地失败关闭并在错误中说明原因，而不会发往 Zen：Gemini（`@ai-sdk/google`，Google 的模型特定协议，Codewhale 不支持）；目录行声明了其他任何包；Models.dev 标记为 `deprecated` 的目录行；两个来源都未列出的模型。目录缺失永远不会回退到另一种 Zen 线协议形态，包括配置了自定义 Zen base URL 时。

### Concentrate 说明

Concentrate（`concentrate`）是一个 OpenAI **Responses** 兼容网关：`POST https://api.concentrate.ai/v1/responses`，使用 `Authorization: Bearer <Universal API 密钥>`。它需要主动启用，且**仅支持自带密钥（BYOK）**。

- **模型 ID 原样通过。** 普通目录 ID（`gpt-5.6-sol`、`deepseek-v4-pro`）由网关选择上游提供商；`provider/model`（`openai/gpt-5.6-sol`）在网关内固定上游提供商；`concentrate/auto` 发送网关自己的 `auto` 路由器。Codewhale 里单独写的 `--model auto` 仍是解析器的哨兵值（使用提供商默认值），所以网关路由器有它自己的显式写法。一旦流式传输开始，网关就会锁定一个提供商（[自动路由](https://concentrate.ai/docs/api-reference/endpoint/auto-routing)）。
- **线协议。** 固定的 Responses 方言。请求只携带文档中记载的字段——`model`、`input`、`stream`、`max_output_tokens`、`tools`、`tool_choice`、`parallel_tool_calls`、`reasoning.effort`——系统提示词作为开头的一个 `system` 输入项传递，因为网关的[参数参考](https://concentrate.ai/docs/api-reference/endpoint/request-parameters)中没有 `instructions`。流式传输遵循带类型的 `response.*` SSE 事件，以 `response.completed` 结束，没有 `[DONE]` 哨兵（[流式传输](https://concentrate.ai/docs/api-reference/endpoint/streaming)）。
- **错误。** 形如 `{ "error", "message", "model"?, "retry_after"? }`，状态码为 400（请求错误/未知模型）、401（密钥无效）、402（额度不足）、424（上游提供商不可用）、429（速率限制）——原样呈现，并按消息内容分类（[错误](https://concentrate.ai/docs/api-reference/endpoint/errors)）。
- **目录。** `GET /v1/models` 不需要密钥（[列出模型](https://concentrate.ai/docs/api-reference/endpoint/list-models)）；实时行按提供商划分，且不作任何声明（不含价格或能力声明）。
- **商业边界。** Concentrate 的[服务条款](https://concentrate.ai/legal/terms-of-service)禁止在未经书面同意的情况下转售、贴牌，或以服务外包的方式提供该服务；其[可接受使用政策](https://concentrate.ai/legal/acceptable-use-policy)禁止共享或转授权密钥。因此 Codewhale **不**随附 Codewhale 自有的 Concentrate 密钥，不保存客户密钥，没有默认或托管路由，也不加价——启用任何托管通道都必须先取得书面同意、条款和计费批准（记录在 Codewhale 运营证据清单 `concentrate-gateway-20260829/CHECKLIST.md` 中）。其公开定价声明为“No platform markup on tokens”。
- **无需密钥的自测。** `scripts/concentrate-selftest.sh` 会启动一个遵循文档约定的本地桩服务，并通过它驱动真实的 `codewhale exec` 路径，断言 URL、Bearer 头、模型直通、流式传输和已完成回合的回执。整个过程不会有任何网络调用离开本机，也不需要账户。

### Hugging Face Provider 与 MCP 与 Hub

Codewhale 的 `huggingface` 提供商 ID 只是通过 Hugging Face Inference Providers 的 OpenAI 兼容聊天推理路由。它用 `/provider huggingface`、`CODEWHALE_PROVIDER=huggingface` 或 `provider = "huggingface"` 选择。

Hugging Face MCP 是单独的外部工具路由。通过 `docs/MCP.md` 中描述的 MCP 配置来配置它，最好使用 <https://huggingface.co/settings/mcp> 生成的设置片段。在 TUI 中，`/hf mcp status` 检查 Hugging Face MCP 服务器是否出现在解析后的 MCP 配置中，`/hf mcp setup` 打印设置工作流和一个仅占位符的形态，`/hf concepts` 解释 provider/MCP/Hub 的区别。

Hub 发布或仓库管理仍然是通过 Hub 原生工具（如 `huggingface_hub` 或 git）的显式用户操作。`/hf` 助手不会上传到 Hugging Face，也不会执行直接的 Hugging Face Hub HTTP 搜索。

### Xiaomi MiMo 说明

`xiaomi-mimo` 默认为 `mimo-v2.5-pro`，用于长上下文推理和编码工作。聊天选择器还暴露 `mimo-v2.5-pro-ultraspeed` 和最新的 Omni 模型 `mimo-v2.5`。Xiaomi MiMo TTS 可通过 `codewhale --provider xiaomi-mimo speech "text" --model tts`（或 `tts` 别名）使用。在 Work 和 Operate 模式中，提供商特定的 `speech` / `tts` 工具在配置了 Xiaomi MiMo 路由时可通过延迟发现使用。

`/provider xiaomi-mimo ultraspeed` 和 `/provider xiaomi-mimo pro-ultraspeed` 都选择 `mimo-v2.5-pro-ultraspeed`。`tts`、`voice-design` 和 `voice-clone` 等语音别名与正常的聊天默认值分开。

Token 套餐密钥默认指向新加坡端点 `https://token-plan-sgp.xiaomimimo.com/v1`。如果你的 MiMo 账户为中国区配置，请在 `[providers.xiaomi_mimo]` 中显式设置 `base_url = "https://token-plan-cn.xiaomimimo.com/v1"` 或设置 `mode = "token-plan-cn"`。欧洲 Token 套餐账户可以设置 `base_url = "https://token-plan-ams.xiaomimimo.com/v1"` 或使用 `mode = "token-plan-ams"`；`mode = "pay-as-you-go"` 选择标准 API 端点和标准 MiMo 密钥族。Xiaomi Token 套餐文档和控制台暴露积分/配额语义，但 Codewhale 目前没有可轮询的文档化余额端点，因此成本显示保持未知，而不是复用另一提供商的 token 价格估算。

Voice-design 和 voice-clone 简写映射到 `mimo-v2.5-tts-voicedesign` 和 `mimo-v2.5-tts-voiceclone`。小米当前的[图像理解指南](https://platform.xiaomimimo.com/docs/en-US/usage-guide/multimodal-understanding/image-understanding)包含用于图像输入的 `mimo-v2.5`。Codewhale 通过单独的 `[vision_model]` / `image_analyze` 路径暴露图像分析；使用 MiMo 做视觉时，把该模型设置为 `mimo-v2.5`。

### OpenRouter 兼容的 Base URL

OpenRouter 兼容网关通常应该留在 `openrouter` 提供商上，使用提供商范围的 `base_url` 覆盖，而不是走通用 `openai` 路由。这样可以让 OpenRouter 风格的推理、流式、缓存使用和命名空间化的线协议模型解析附着在所选路由上：

```toml
provider = "openrouter"

[providers.openrouter]
api_key = "sk-..."
base_url = "https://openrouter-compatible.example/v1"
model = "deepseek/deepseek-v4-pro"
```

Codewhale 在 OpenRouter 提供商范围内保留 `deepseek/` 线协议模型前缀；它不会从该模型字符串推断切换到直接 DeepSeek 提供商。当上游网关发送 `prompt_cache_hit_tokens`、`prompt_cache_miss_tokens` 和 `prompt_tokens_details.cached_tokens` 等缓存字段时，会解析它们。如果某个密钥/账户类型省略了这些字段，Codewhale 将该响应视为这些字段不存在，而不是不同的提供商路由。

OrcaRouter（`https://api.orcarouter.ai/v1`）是专用的具名路由（[OrcaRouter](https://www.orcarouter.ai)），它说同样的 OpenAI Chat Completions 线协议，并提供同样的命名空间化 `vendor/model` 目录。它不需要 OpenRouter 兼容的 `base_url` 覆盖：选择 `provider = "orcarouter"`，它的命名空间化线协议模型（例如 `deepseek/deepseek-v4-pro` 或它自己的 `orcarouter/auto` 路由器）会原样通过，就像它们在 OpenRouter 提供商范围上一样。

#### OrcaRouter：API key 或 OAuth 2.0 + PKCE

已有 key 可使用 `codewhale auth set --provider orcarouter`、`/provider` 或
`ORCAROUTER_API_KEY`。`codewhale auth orcarouter` / `/auth orcarouter` 通过
PKCE S256、回环回调与恒定时间 state 校验获得持久 `sk-orca-…` key；两条路径写入
同一个 `orcarouter` 秘密槽位，`auth_mode = "api_key"`。

浏览器授权为 `https://www.orcarouter.ai/auth`；交换为
`POST https://www.orcarouter.ai/api/v1/auth/keys`，无需 client secret 或预注册
回调 URI。推理和目录使用独立的 `https://api.orcarouter.ai/v1`，不能由另一个
源站猜测路径。自托管可显式用 `ORCA_AUTH_BASE_URL` 与
`ORCA_API_BASE_URL` / `ORCAROUTER_BASE_URL` 覆盖各自源站；远程必须 HTTPS，
HTTP 只允许回环。

所得 key 不是刷新令牌：重启后复用，不发送 refresh grant。请在 OrcaRouter
控制台撤销；`/auth orcarouter-revoke` 只清除本地副本。Relay 401 将该凭据版本
标为需重新认证，不静默重试。

实时 `GET https://api.orcarouter.ai/v1/models` 用所配置 key 获取目录。
聊天选择器仅保留 `supported_endpoint_types` 含 `openai`、`anthropic`、`gemini`
或 `openai-response` 的行；图像生成、视频与 rerank 行被过滤，不按名称猜测。
`architecture.input_modalities` 含 `image` 才确认图像输入；无 architecture 的行
能力为未知。

### 近期 OpenRouter 大模型

OpenRouter completions 和静态注册表行包括自 2026 年 4 月起通过 OpenRouter 模型元数据验证的大模型：`arcee-ai/trinity-large-thinking`、`qwen/qwen3.6-flash`、`qwen/qwen3.6-35b-a3b`、`qwen/qwen3.6-max-preview`、`qwen/qwen3.6-27b`、`qwen/qwen3.6-plus`、`minimax/minimax-m3`、`xiaomi/mimo-v2.5-pro`、`xiaomi/mimo-v2.5`、`moonshotai/kimi-k2.7-code`、`moonshotai/kimi-k2.6`、`z-ai/glm-5.1`、`z-ai/glm-5.2`、`z-ai/glm-5-turbo`、`tencent/hy3-preview`、`google/gemma-4-31b-it`、`google/gemma-4-26b-a4b-it` 和 `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`。`minimax/minimax-m3` 是从 OpenRouter 2026 年 5 月 31 日的列表中新增的，作为 1M 上下文的多模态模型，用于编码、工具调用和长周期的智能体工作。`GLM-5.3` 现在是直接 Z.AI Coding 套餐的默认模型；`GLM-5.2` / `z-ai/glm-5.2` 仍然可用（显式选择保留自己的 id），`GLM-5.1` / `z-ai/glm-5.1` 作为更小的模型仍然可用，`GLM-5.3-Flash` / `z-ai/glm-5.3-flash` 是 `GLM-5.3` 的更快的探索型同系模型，`GLM-5-Turbo` / `z-ai/glm-5-turbo` 则仍是 `GLM-5.2` 的更快的同系模型。`GLM-5.3` / `z-ai/glm-5.3` 与 `GLM-5.3-Flash` / `z-ai/glm-5.3-flash` 是 Z.ai 和 OpenRouter 路由上的一等选择器 id（先执行 `/provider zai` 再用 `/model`，或设置 `model = "GLM-5.3-Flash"`）。在 Z.ai 发布不同的 5.3 元数据之前，5.3 的限制和推理选项继承自 `GLM-5.2`，而且 5.3 不带价格。Flash 采用已公布的 $0.15/$0.50 价目。在未开通 5.3 的账户上，实时调用仍可能以权限代码 1311 返回 429。

## 静态模型注册表

下表描述的是静态注册表；`codewhale model resolve`（以及 `[model]` 配置面）仍用它解析选择器别名。这与实时 `/models` 发现不同。当端点支持模型列表时，使用 `/models` 或 `codewhale models` 从活动的 API 端点获取模型 ID。

| 提供商 | 静态注册表条目 | 工具调用 | 注册表推理标志 |
| --- | --- | --- | --- |
| `deepseek` | `deepseek-v4-pro`, `deepseek-v4-flash`, `deepseek-v4-flash-vision-exp` | 是 | 是 |
| `nvidia-nim` | `deepseek-ai/deepseek-v4-pro`, `deepseek-ai/deepseek-v4-flash` | 是 | 是 |
| `openai` | `deepseek-v4-pro`, `deepseek-v4-flash`, `gpt-5.6`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` | 是 | 是 |
| `atlascloud` | `deepseek-ai/deepseek-v4-flash`, `deepseek-ai/deepseek-v4-pro` | 是 | 是 |
| `wanjie-ark` | `deepseek-reasoner` | 是 | 是 |
| `volcengine` | `DeepSeek-V4-Pro`, `DeepSeek-V4-Flash` | 是 | 是 |
| `openrouter` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash`, `arcee-ai/trinity-large-thinking`, `minimax/minimax-m3`, `minimax/minimax-m2.7`, `xiaomi/mimo-v2.5-pro`, `xiaomi/mimo-v2.5`, `qwen/qwen3.6-flash`, `qwen/qwen3.6-35b-a3b`, `qwen/qwen3.6-max-preview`, `qwen/qwen3.6-27b`, `qwen/qwen3.6-plus`, `qwen/qwen3.7-max`, `moonshotai/kimi-k2.7-code`, `moonshotai/kimi-k2.6`, `z-ai/glm-5.1`, `z-ai/glm-5.2`, `z-ai/glm-5.3`, `z-ai/glm-5.3-flash`, `z-ai/glm-5-turbo`, `tencent/hy3-preview`, `google/gemma-4-31b-it`, `google/gemma-4-26b-a4b-it`, `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`, `nvidia/nemotron-3-ultra-550b-a55b` | 是 | 是 |
| `orcarouter` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash`, `orcarouter/auto` | 是 | 是 |
| `xiaomi-mimo` | `mimo-v2.5-pro`, `mimo-v2.5-pro-ultraspeed`, `mimo-v2.5`；语音/TTS ID 通过 `codewhale speech` / `tts` 选择 | 是 | 聊天模型是；语音/TTS 模型否 |
| `novita` | `deepseek/deepseek-v4-pro`, `deepseek/deepseek-v4-flash` | 是 | 是 |
| `fireworks` | `accounts/fireworks/models/deepseek-v4-pro` | 是 | 是 |
| `siliconflow` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | 是 | 是 |
| `arcee` | `trinity-large-thinking`, `trinity-large-preview`；提供商提示的自定义模型 ID 原样通过 | 是 | `trinity-large-thinking` 是；`trinity-large-preview` 否 |
| `moonshot` | `kimi-k2.7-code`, `kimi-k2.6` | 是 | 是 |
| `zai` | `GLM-5.3`, `GLM-5.3-Flash`, `GLM-5.2`, `GLM-5.1`, `GLM-5-Turbo`；提供商提示的自定义模型 ID 原样通过 | 是 | 是 |
| `stepfun` | `step-3.7-flash`（默认）, `step-5-preview`, `step-3.5-flash`, `step-3.5-flash-2603` | 是 | 是 |
| `minimax` | `MiniMax-M3`, `MiniMax-M2.7`, `MiniMax-M2.7-highspeed`, `MiniMax-M2.5`, `MiniMax-M2.5-highspeed`, `MiniMax-M2.1`, `MiniMax-M2.1-highspeed`, `MiniMax-M2` | 是 | 是 |
| `minimax-anthropic` | `MiniMax-M3`, `MiniMax-M2.7` | 是 | 是 |
| `sglang` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | 是 | 是 |
| `vllm` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | 是 | 是 |
| `ollama` | 实况本地标签；提供商提示为 `ollama` 时自定义标签原样通过 | 是 | 否 |
| `ollama-cloud` | `gpt-oss:120b`；任意的提供商自有模型 ID 原样通过 | 是 | 是 |
| `huggingface` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | 是 | 否 |
| `modelscope` | `Qwen/Qwen3.5-397B-A17B`, `Qwen/Qwen3.5-122B-A10B`, `Qwen/Qwen3.5-27B`, `Qwen/Qwen3.5-35B-A3B`, `Qwen/Qwen3.8-27B`, `Qwen/Qwen3.8-Flash-Next`, `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Pro-0813`, `deepseek-ai/DeepSeek-V4.1-Flash`, `ZhipuAI/GLM-4.7-Flash`, `ZhipuAI/GLM-5.2` | 是 | 否 |
| `deepinfra` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash` | 是 | 是 |
| `together` | `deepseek-ai/DeepSeek-V4-Pro`, `deepseek-ai/DeepSeek-V4-Flash`, `thinkingmachines/inkling` | 是 | 是 |
| `openai-codex` | `gpt-5.5` | 是 | 是 |
| `anthropic` | `claude-opus-5`, `claude-opus-4-8`, `claude-sonnet-5`, `claude-sonnet-4-6`, `claude-fable-5`, `claude-haiku-4-5` | 是 | 除 `claude-haiku-4-5` 外都是 |
| `openmodel` | `deepseek-v4-flash`；提供商范围的自定义模型 ID 原样通过 | 是 | 取决于模型 |
| `sakana` | `fugu`, `fugu-ultra-20260615` | 是 | `fugu-ultra-20260615` 是 |
| `longcat` | `LongCat-2.0` | 是 | 是 |
| `opencode-go` | `deepseek-v4-pro`, `grok-4.5`, `glm-5.2`, `glm-5.1`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `deepseek-v4-flash`, `mimo-v2.5`, `mimo-v2.5-pro` | 是 | 是 |
| `meta` | `muse-spark-1.2` | 是 | 是 |
| `xai` | `grok-4.6`, `grok-4.7`, `grok-4.5`, `grok-4.3`, `grok-build`, `grok-composer-2.5-fast`, `grok-4.20-0309-reasoning`, `grok-4.20-0309-non-reasoning` | 是 | `grok-4.6`, `grok-4.7`, `grok-4.5`, `grok-4.3`, `grok-build` 和 `grok-4.20-0309-reasoning` 是 |
| `google` | `gemini-3.1-pro-preview`, `gemini-3-pro-preview`, `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-2.5-pro`, `gemini-2.5-flash` | 是 | 除 `gemini-3.5-flash-lite` 外都是 |
| `mistral` | `mistral-code-latest`, `mistral-medium-latest`, `mistral-small-latest`, `mistral-large-latest` | 是 | Medium 和 Small 是（在精确的第一方路由上 `reasoning_effort` 为 `none` 或 `high`）；已弃用的原生 Magistral 仍然是始终启用的显式兼容 ID；Code 和 Large 否 |
| `modelstudio-token-plan`, `modelstudio-coding-plan` | `qwen3.8-max`, `qwen3.8-max-preview`, `qwen3.7-plus`, `qwen3.7-max`, `qwen3.6-flash`, `deepseek-v4-pro`, `deepseek-v4-flash-0731`, `glm-5.2` | 是 | 是 |
| `csdn` | `glm_for_coding`；其他市场模型 ID 原样通过 | 是 | 是 |

AtlasCloud 与配置层保持相同的默认模型，并为 Pro 和 Flash 行添加提供商范围的别名。其他 AtlasCloud 模型 ID 仍应通过 `ATLASCLOUD_MODEL`、配置或可用的实时模型列表来选择。

## 能力元数据

`codewhale doctor --json` 暴露 `capability` 对象。它是静态元数据，不是实时 API 探测。当前字段是：

`resolved_provider`, `resolved_model`, `context_window`, `max_output`,
`thinking_supported`, `cache_telemetry_supported` 和 `request_payload_mode`。

当配置无法加载或验证时，`doctor --json` 以非零退出，并打印一个有界、密钥已打码的 JSON 错误包，`status = "error"` 且 `error.kind = "config_validation"`，而不是发出误导性的路由或能力元数据。

大多数随附提供商使用 Chat Completions 请求负载模式。原生 Messages 路由（包括 `minimax-anthropic`）使用 `/v1/messages`，而 `openai-codex` 使用 Responses。

对于实际窗口与静态表不同的 OpenAI 兼容网关或自托管运行时，设置 `[providers.<name>] context_window = N`。配置的值会成为提示、上下文压力检查、压缩和输出预算的生效路由上下文窗口。

`max_output` 是可选的，并且如实反映：当路由没有发布我们能够担保的输出上限时，它是 `null`（并且在线上传输的 capability 结构中被省略）——Kimi Code 会员的 `kimi-for-coding` 系列就是典型例子，因为它们的限制由会员目录掌握。未知的输出上限绝不会用占位值回填，也**不会**对某个回合请求的 `max_tokens` 施加任何兼容性钳制；只有具体的路由/提供商上限才会收窄请求。目录里根本没有某个模型的行则是另一回事——没有记录不等于被允许，所以未编入目录的 id 会保持一个保守的上限。下表的“Max output 元数据”一列，凡是没有文档记载上限之处，都显示 `unknown`。

精确的 Kimi Code 会员名册包含 `k3`、`k3-256k`、`kimi-for-coding` 和 `kimi-for-coding-highspeed`。两个 K3 id 共享相同的推理和固定采样约定；`k3-256k` 固定为 262,144 token，而裸 `k3` 可以使用已获授权的 1M 覆盖。

| 提供商/模型类别 | 上下文窗口 | Max output 元数据 | 思考支持 | 缓存遥测 | FIM 端点 |
| --- | --- | --- | --- | --- | --- |
| DeepSeek V4（`deepseek-v4-pro`, `deepseek-v4-flash`） | 1,000,000 | 384,000 | 是 | 是 | 仅 DeepSeek beta |
| DeepSeek V4 Flash Vision 实验版（`deepseek-v4-flash-vision-exp`） | 继承自 Flash，1,000,000 | 继承自 Flash，384,000 | 是，继承 | 是，继承 | 未声明；仅限 Chat Completions 路由 |
| DeepSeek 兼容别名（`deepseek-chat`, `deepseek-reasoner`） | 1,000,000 | 384,000 | 是 | 是 | 仅 DeepSeek beta |
| NVIDIA NIM V4 注册表模型 | 1,000,000 | 384,000 | 是 | 是 | 代码中未记录 |
| Volcengine Ark V4 模型 ID | 1,000,000 | 384,000 | 是 | 是 | 代码中未记录 |
| OpenRouter、Novita、Fireworks、SiliconFlow、SGLang 和 vLLM V4 模型 ID | 1,000,000 | 384,000 | 是 | 否 | 代码中未记录 |
| Xiaomi MiMo `mimo-v2.5-pro`, `mimo-v2.5-pro-ultraspeed`, `mimo-v2.5` | 1,000,000 | 131,072 | 是 | 否 | 代码中未记录 |
| OpenRouter Qwen 3.6 Flash / Plus | 1,000,000 | 65,536 | 是 | 否 | 代码中未记录 |
| OpenRouter Qwen 3.6 35B / 27B | 262,144 | 262,140 | 是 | 否 | 代码中未记录 |
| OpenRouter Qwen 3.6 Max Preview | 262,144 | 65,536 | 是 | 否 | 代码中未记录 |
| OpenAI API `gpt-5.5` | 1,050,000 | 128,000 | 是 | 否 | 代码中未记录 |
| OpenAI API `gpt-5.6`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna` | 1,050,000 | 128,000 | 是 | 否 | 代码中未记录 |
| Anthropic API `claude-opus-5`, `claude-opus-4-8`, `claude-sonnet-5`, `claude-sonnet-4-6`, `claude-fable-5` | 1,000,000 | 128,000 | 是 | 是 | 代码中未记录 |
| Google Gemini API `gemini-3.7-flash`, `gemini-3.6-flash`, `gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-3.1-pro-preview`, `gemini-2.5-pro`, `gemini-2.5-flash` | 1,048,576 | 65,536 | 取决于模型 | 否 | 代码中未记录 |
| Meta Model API `muse-spark-1.2` | 1,000,000 | 32,000 | 是 | 否 | 代码中未记录 |
| ChatGPT 套餐路由（`openai-codex`） | 保守预算；账户列表不提供窗口大小 | 预览请求不发送输出上限 | 取决于模型 | 否 | 公共 `/v1/responses`；不从账户访问权限推断上下文上限 |
| OpenModel 默认/自定义模型 ID | 回退 200,000，除非模型元数据或配置覆盖 | 回退 64,000 | 取决于模型 | 否 | 路由在 `/v1/messages` 使用 Messages 负载 |
| Wanjie Ark `reasoner` / `r1` 模型 ID | 128,000 | 未知（无文档化上限） | 是 | 否 | 代码中未记录 |
| 直接 Arcee API `trinity-large-thinking` | 262,144 | 262,144 | 是 | 否 | 代码中未记录 |
| 直接 Arcee API `trinity-large-preview` | 262,144 | 未知（无文档化上限） | doctor 能力元数据中否 | 否 | 代码中未记录 |
| 直接 Moonshot `kimi-k3` | 1,048,576 | 文档化上限 1,048,576；提供商默认 131,072 | 是 | 否 | 精确路由使用 `max_completion_tokens` 并省略固定采样字段（[K3 快速入门](https://platform.kimi.ai/docs/guide/kimi-k3-quickstart)） |
| Kimi Code 会员 `k3` | 安全基线 262,144；显式设置已获授权套餐的覆盖后为 1,048,576 | 保守默认上限 131,072；会员上限未发布 | 是 | 否 | 精确 `https://api.kimi.com/coding/v1` 路由 |
| Kimi Code 会员 `k3-256k` | 262,144 固定 | 保守默认上限 131,072；会员上限未发布 | 是 | 否 | 精确 `https://api.kimi.com/coding/v1` 路由 |
| 直接 Moonshot/Kimi K2.7/K2.6（`kimi-k2.7-code`, `kimi-k2.7-code-highspeed`, `kimi-k2.6`） | 262,144 | 32,768 | 是 | 否 | 提供商报告的捆绑目录 |
| Kimi Code 会员 `kimi-for-coding`, `kimi-for-coding-highspeed` | 262,144 | 未知——会员目录拥有这些限制，不声称客户端侧上限 | 是 | 否 | 精确 `https://api.kimi.com/coding/v1` 路由 |
| 直接 Z.AI `GLM-5.3`（默认） | 1,000,000 | 131,072 | 是 | 否 | GLM Coding 套餐上实时可用；限制继承自 `GLM-5.2`，直到 Z.ai 发布不同的 5.3 数字；无 USD 价格 |
| 直接 Z.AI `GLM-5.3-Flash` | 1,000,000 | 131,072 | 是 | 否 | 原生多模态；$0.15/$0.50 价目（2026-08-26）；`GLM-5.3` 的更快的探索型同系模型 |
| 直接 Z.AI `GLM-5.2` | 1,000,000 | 131,072 | 是 | 否 | 代码中未记录 |
| 直接 Z.AI `GLM-5.1` | 202,752 | 131,072 | 是 | 否 | 代码中未记录 |
| 直接 Z.AI `GLM-5-Turbo` | 202,752 | 131,072 | 是 | 否 | 供 faster/explore 智能体使用的同系模型 |
| 直接 MiniMax `MiniMax-M3` | 1,000,000 | 524,288 | 是 | 否 | 代码中未记录 |
| 直接 MiniMax M2.x 模型 | 204,800 | 未知，直到 MiniMax output 元数据被提升 | 是 | 否 | 代码中未记录 |
| MiniMax Messages 路由（`MiniMax-M3`, `MiniMax-M2.7`） | 上面的模型特定值 | 上面的模型特定值 | 是 | 否 | 路由使用 `/anthropic/v1/messages` |
| 通用 `openai` 和 AtlasCloud | 128,000 | 未知（无文档化上限） | doctor 能力元数据中否 | 否 | 代码中未记录 |
| Ollama | 8,192 | 未知（无文档化上限） | 否 | 否 | 代码中未记录 |
| Hugging Face Inference Providers V4 模型 ID | 131,072 | 未知（无文档化上限） | 是 | 否 | 代码中未记录 |
| 其他已识别的 DeepSeek 模型 ID | 128,000，除非模型名带显式 `Nk` 提示 | 未知（无文档化上限） | 除非 V4/reasoner 逻辑匹配，否则否 | 仅 DeepSeek/NIM | 仅 DeepSeek beta |

MiniMax M3 使用输入长度和服务层级。Codewhale 省略 `service_tier`，因此请求使用标准层级，成本估算从总输入用量中选择正确的标准费率。优先费率被列出以保持官方层级结构可见。价格为每百万 token 的美元。

| 模型 / 服务层级 | 输入长度 | 输入 | 输出 | 缓存读取 | 缓存写入 |
| --- | --- | ---: | ---: | ---: | ---: |
| `MiniMax-M3` 标准 | 最多 512,000 个输入 token | $0.30 | $1.20 | $0.06 | 未发布 |
| `MiniMax-M3` 标准 | 超过 512,000 个输入 token | $0.60 | $2.40 | $0.12 | 未发布 |
| `MiniMax-M3` 优先 | 最多 512,000 个输入 token | $0.45 | $1.80 | $0.09 | 未发布 |
| `MiniMax-M3` 优先 | 超过 512,000 个输入 token | $0.90 | $3.60 | $0.18 | 未发布 |
| `MiniMax-M2.7` 标准 | 所有受支持的输入 | $0.30 | $1.20 | $0.06 | $0.375 |

这些值来自 [MiniMax 按量付费定价指南](https://platform.minimax.io/docs/guides/pricing-paygo)。M3 思考是自适应的或禁用；OpenAI 兼容 API 默认为自适应，Anthropic 兼容 API 默认为禁用。M2.7 思考无法禁用。当用户选择推理模式时，Codewhale 发送显式控制。

工具调用支持由静态 `ModelRegistry` 和端点接受 OpenAI 兼容 `tools` 负载的能力分别跟踪。自定义 OpenAI 兼容或本地端点仍然可以拒绝工具调用，即使 Codewhale 能够发送该模式。

### Hugging Face Inference Providers 说明

随附的 Hugging Face 路由指向 `https://router.huggingface.co/v1` 的 OpenAI 兼容 Inference Providers 路由器。先用 `HUGGINGFACE_API_KEY` 配置认证，或回退到 `HF_TOKEN`。先用 `HUGGINGFACE_BASE_URL` 配置端点，或回退到 `HF_BASE_URL`；先用 `HUGGINGFACE_MODEL` 配置模型，或回退到 `HF_MODEL`。

此路由不意味着 Hub 浏览、模型卡元数据、数据集访问、Jobs、上传或导出。那些仍然是显式的 Model Lab 工作项，以便提供商认证和工件移动保持分离。

### 当本地模型输出工具 JSON 时

只有提供商返回 Chat Completions `tool_calls` 或流式的 `delta.tool_calls` 时，Codewhale 才会执行工具。如果本地模型在助手消息中打印诸如 `{"name":"File","arguments":{"action":"search_content",...}}` 之类的文本，那是普通的模型输出，不是可执行的工具请求。

对于 OpenAI 兼容或本地运行时，请检查：

- 端点接受 `/v1/chat/completions` 请求中的 `tools` 数组。
- 所选模型或聊天模板已为 function/tool 调用配置。
- 服务器在响应中返回 `tool_calls`，而不是普通 JSON 文本。
- 兼容层不会在转发请求前剥离 tools。
- 如有疑问，在调试 Codewhale 的工具注册表之前，先针对已知的、支持工具调用的模型测试一个小的 `File` `read` 或 `search_content` 动作。

更改 `provider`、`base_url` 或 `model` 可以选择支持 OpenAI 兼容负载形态的路由，但 Codewhale 无法在模型已将其作为散文发出之后，把任意的 JSON 文本转换成受信的工具调用。

DeepSeek 将于 2026-07-24 15:59 UTC 退役 `deepseek-chat` 和 `deepseek-reasoner`。在请求到达 DeepSeek 第一方 OpenAI 或 Anthropic 端点之前，Codewhale 会把任一名称迁移到 `deepseek-v4-flash`。如果未配置推理层级，`deepseek-chat` 也会迁移到 `off`，`deepseek-reasoner` 迁移到 `high`，保留它们原先的非思考/思考意图；显式的 `reasoning_effort` 仍然权威。该映射刻意不是全局的：Wanjie Ark、聚合器、自托管运行时和自定义端点继续拥有它们自己的模型 id。

## 推理强度

`/reasoning <effort>`（以及 `reasoning_effort` 配置键）会在请求发送前由客户端翻译成每个提供商的线协议方言。`off` 在路由支持的地方禁用思考。两个精确的 K3 路由都把 `off` 映射到它们支持的最低层级 `low`，而且模型绝不会为了满足 `off` 而切换——但它们这样做的原因不同：

- **Kimi Code 会员 K3**（精确 `https://api.kimi.com/coding/v1`，`model = "k3"` 或 `model = "k3-256k"`）——会员名册声明 K3 始终思考，所以在不改变模型本身的前提下无法满足 `off`。这一钳制保留了固定的 K3 身份。
- **直接 Moonshot K3**（精确 `https://api.moonshot.ai/v1`，`model = "kimi-k3"`）——这个钳制是*防御性的*，并非文档记载的约定。直连平台没有为 K3 公布 `off` 状态，而 Codewhale 也不会声称一个它无法针对某个密钥的授权情况去验证的固定思考保证，因此请求的 `off` 会被规范化为最低层级，实际的授权情况保持未知。

正常分派的 `auto` 使用 Codewhale 的自动推理选择器并发送具体的层级；只有省略推理设置才会让提供商默认值控制。标记为 "omitted" 的提供商在该层级根本不会收到推理字段。

| 提供商 | `off` | `low`/`medium`/`high` | `max`/`xhigh` |
| --- | --- | --- | --- |
| `deepseek`, `deepseek-cn`, `siliconflow`, `siliconflow-CN`, `sglang`, `volcengine`, `atlascloud` | `thinking: {type: disabled}` | `reasoning_effort: "high"` + `thinking: {type: enabled}` | `reasoning_effort: "max"` + `thinking: {type: enabled}` |
| `openrouter`, `novita`, 其他 `together` 模型 | `thinking: {type: disabled}` | `reasoning_effort` 直通 + `thinking: {type: enabled}` | `reasoning_effort: "xhigh"` + `thinking: {type: enabled}` |
| `together` + `thinkingmachines/inkling` | `reasoning_effort: "none"` | 精确的 `minimal`/`low`/`medium`/`high` `reasoning_effort` | `reasoning_effort: "max"` |
| 精确 `https://api.moonshot.ai/v1` 上的直接 Moonshot `kimi-k3` | 顶层 `reasoning_effort: "low"`（生效的规范化） | 顶层 `reasoning_effort: "low"` / `"high"`（`medium` 变成 `high`） | 顶层 `reasoning_effort: "max"` |
| 精确 `https://api.kimi.com/coding/v1` 上的 Kimi Code 会员 `k3`, `k3-256k` | `thinking: {type: enabled, effort: "low"}`（生效的规范化） | `thinking: {type: enabled, effort: "low" \| "high"}` | `thinking: {type: enabled, effort: "max"}` |
| 其他 `moonshot` 路由 | `thinking: {type: disabled}` | `thinking: {type: enabled}` | `thinking: {type: enabled}` |
| `ollama` | `think: false` | `think: true` | `think: true` |
| `ollama-cloud` | `reasoning_effort: "none"` | 精确的 `low`/`medium`/`high` `reasoning_effort` | `reasoning_effort: "max"` |
| `xiaomi-mimo` | `thinking: {type: disabled}` | `thinking: {type: enabled}` | `thinking: {type: enabled}` |
| 第一方 `minimax` `MiniMax-M3` | `reasoning_split: true` + `thinking: {type: disabled}` | `reasoning_split: true` + `thinking: {type: adaptive}`；生效的层级粒度不可用 | `reasoning_split: true` + `thinking: {type: adaptive}`；生效的层级粒度不可用 |
| 第一方 Z.ai `GLM-5.2` | `thinking: {type: disabled}`；无 `reasoning_effort` | 启用思考；只有生效的 `high` 才添加 `reasoning_effort: "high"` | 启用思考 + `reasoning_effort: "max"` |
| 第一方 Z.ai `GLM-5.3` | `thinking: {type: disabled}`；无 `reasoning_effort` | 启用思考；只有生效的 `high` 才添加 `reasoning_effort: "high"` | 启用思考 + `reasoning_effort: "max"` |
| 第一方 Z.ai `GLM-5.3-Flash` | `thinking: {type: disabled}`；无 `reasoning_effort` | 启用思考；只有生效的 `high` 才添加 `reasoning_effort: "high"` | 启用思考 + `reasoning_effort: "max"` |
| 第一方 Z.ai `GLM-5-Turbo` | `thinking: {type: disabled}` | 启用思考；effort 粒度不可用 | 启用思考；effort 粒度不可用 |
| 配置为 `zai` 的兼容网关 | omitted；生效值不可用 | omitted；生效值不可用 | omitted；生效值不可用 |
| `nvidia-nim` | `chat_template_kwargs.thinking: false` | `chat_template_kwargs`：`thinking: true` + `reasoning_effort: "high"` | `chat_template_kwargs`：`thinking: true` + `reasoning_effort: "max"` |
| `vllm` | `chat_template_kwargs.enable_thinking: false` | `chat_template_kwargs.enable_thinking: true` + `reasoning_effort` low/medium/high | `chat_template_kwargs.enable_thinking: true` + `reasoning_effort: "high"`（vLLM 没有 max 层级） |
| `arcee`, `huggingface` | omitted | `reasoning_effort` 直通 | `reasoning_effort: "high"` |
| `fireworks` | omitted | `reasoning_effort: "high"` | `reasoning_effort: "max"` |
| `openai`, `wanjie-ark`, `telecomjs` | omitted | omitted | omitted |
| `openmodel` | Anthropic Messages 适配器处理思考/输出配置 | Anthropic Messages 适配器处理思考/输出配置 | Anthropic Messages 适配器处理思考/输出配置 |
| `openai-codex` | Responses API `reasoning` 字段（由 Responses 桥处理） | Responses API `reasoning` 字段 | Responses API `reasoning` 字段 |

AtlasCloud 提供 DeepSeek 模型，因此它说 DeepSeek 推理方言，包括 `max` 层级（#3024）。

在精确的 MiniMax OpenAI 兼容 Chat 端点上，`MiniMax-M3` 使用 `max_completion_tokens`。其他 MiniMax 模型和兼容网关保留 `max_tokens`；MiniMax Anthropic 端点使用单独的 Messages 适配器。

## 漂移检查

在更改提供商 ID、提供商 TOML 表、静态模型注册表行或提供商默认字符串之前运行它：

```bash
python3 scripts/check-provider-registry.py
```

以下情况检查会失败：

- `docs/PROVIDERS.md` 遗漏了一个规范的 `ProviderKind::as_str()` ID。
- `crates/tui/src/config.rs` 的 `ApiProvider::as_str()` 偏离 `ProviderKind::as_str()`，除了显式的 `deepseek-cn` 旧别名。
- 随附提供商表遗漏或添加了一个 `[providers.*]` TOML 表。
- 静态模型注册表表偏离 `crates/agent/src/lib.rs` 使用的提供商。
- `crates/tui/src/config.rs` 中的提供商默认模型或 base URL 常量在这里不再被提及。

## 提供商常见陷阱

提供商运营方在真实流量上测得的三种“看起来像别的问题”的故障：

- **冷模型看起来像坏了。** 对提供商尚未加载的模型，首次调用可能需要 90 到 150 秒，预热后降到一秒甚至更短。在把超时当作“模型不可用”之前，先调大 `[stream].open_timeout_secs`（默认 45 秒，最多 300 秒）。
- **列出不等于可调用。** 目录里可见的条目仍可能每次调用都失败。若每次尝试都返回相同的错误文本（例如“resources busy”的 502），那不是暂时性故障：真正繁忙的上游会在其间返回成功。使用模型前先探测，并停止重试每次都以相同方式失败的模型。
- **被截断的回答可能报告 `stop`。** 推理模型可能把整个 `max_tokens` 额度花在推理上。当提供商报告的 `completion_tokens` 恰好等于请求的 `max_tokens`，且结束原因为 `stop` 时，Codewhale 把该回合视为在上限处被截断而非已完成（`codewhale-models` 中的 `stop_contradicts_output_ceiling`）；用相同提示重试会在相同数量处停止，因此应调高上限或缩短任务。提供商未报告用量时不做任何推断。

## 已规划、尚未随附

这些项目属于 v0.8.48+ 提供商抽象里程碑或相关的提供商文档工作，但它们不是本 checkout 中原生随附的行为：

- `codewhale-agent` 中一个统一的 `Provider` trait，拥有环境变量优先级、密钥解析、base URL 规范化、认证头构造和提供商元数据。这些职责目前仍然分散在 `crates/config`、`crates/secrets` 和 `crates/tui/src/client.rs` 中。
- 选择器中的 Hugging Face 模型护照元数据，包括许可证、基础模型、上下文长度、聊天模板、工具调用支持、推理支持以及 gated/private 状态。
