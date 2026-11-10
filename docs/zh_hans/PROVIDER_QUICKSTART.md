# 提供商快速上手：从装好到收到第一个回答

> 本页为中文新手入门页，内容摘自英文文档 [PROVIDERS.md](../PROVIDERS.md)、[INSTALL.md](../INSTALL.md) 等；如有出入，以英文原版为准。
> 最后核对日期：2026-11-10。

本页只讲一件事：Codewhale 装好以后，怎么连上一个模型，让它回答你的第一句话。
其他配置都不用现在学。看完本页，再按最后一节去读完整参考。

还没安装的话，先看 [INSTALL.md](./INSTALL.md)；Windows 用户可以先看 [WINDOWS_BEGINNER.md](./WINDOWS_BEGINNER.md)。

---

## 1. 先弄懂三个词

- **提供商（provider）**：提供模型服务的公司或平台，例如 DeepSeek、Kimi（月之暗面）、硅基流动；也可以是你电脑上的本地程序，例如 Ollama。你平时说的「渠道」，在 Codewhale 里就叫提供商。每个提供商有一个英文 ID（如 `deepseek`、`moonshot`），命令和配置文件里都用这个 ID。
- **模型（model）**：真正回答问题的 AI。一个提供商通常有好几个模型，每个模型有自己的模型 ID（如 `deepseek-flash`、`deepseek-v4-pro`）。
- **API 密钥（API key）**：你在提供商网站上创建的一串字符。Codewhale 带着它去调用模型，费用由提供商直接向你收取。本地模型不需要密钥。

另外，每个提供商还有一个接口地址（`base_url`，界面里叫「基础 URL」）。内置提供商已经填好了，一般不用改。

一句话：**选一个提供商 → 给它 API 密钥 → 选它的一个模型。**

## 2. 最快路径：在界面里配好 DeepSeek

DeepSeek 是默认提供商。下面以它为例，其他提供商的步骤完全一样。

1. 打开 DeepSeek 开放平台，创建一个 API 密钥：<https://platform.deepseek.com/api_keys>。
2. 在你要干活的文件夹里运行 `codewhale`。没有密钥也能进入界面。
3. 输入 `/provider` 并回车，或者直接按 `F3`，打开提供商选择器。有的终端收不到功能键，这时用 `/provider`。
4. 用方向键选中 **DeepSeek**，按回车。还没配密钥的提供商，旁边会标 `needs key`。列表很长时，先按 `/` 再输入名字搜索。
5. 粘贴你的密钥（显示为打码），按回车。Codewhale 会先在线检查这个密钥；检查不通过时，会停在这一步并显示提供商返回的错误，错误的密钥不会被保存。
6. 选一个默认模型。提供商的默认模型已经帮你选好（DeepSeek 是 `deepseek-flash`），拿不准就直接回车。
7. 最后是确认页，列出提供商、打码后的密钥和模型。按回车才会保存；在这之前什么都不写入。保存后，这个密钥在所有文件夹里都能用。选的不是 DeepSeek 时，再输入 `/model save-default`，把它存为启动默认值。
8. 在输入框里打一句话试试，比如「你好，用一句话介绍你自己」。有回答，就说明配好了。

**另一种开始方式**：第一次启动时，你也可以直接打字发送。Codewhale 会在你发第一条消息时请你选提供商，你的消息会留在输入框里，配好后再发出去。这一步的标题是「选择模型提供商」；如果只想先看看界面，可以按 `Ctrl+O` 离线浏览，之后随时用 `/provider` 连接。

**不想在界面里操作**，也可以在终端里保存密钥：

```bash
codewhale auth set --provider deepseek
```

它会提示你输入密钥。换成别的提供商，只要把 `deepseek` 换成对应的提供商 ID。

## 3. 没有 API 密钥：用本地模型（Ollama）

本地运行的模型默认不需要密钥。Codewhale 内置了 `ollama`、`vllm`、`sglang` 等本地提供商。以 Ollama 为例：

- 如果你还没配置任何提供商，而本机的 Ollama 正在运行、并且已经有聊天模型，Codewhale 会在你发第一条消息时提出使用它。只有本机 Ollama 服务有响应时，才会出现这个选项。
- 想手动指定，就先启动 Ollama、下载一个模型，再让 Codewhale 用它：

```bash
ollama serve          # 如果 Ollama 还没在运行
ollama pull <model>   # <model> 换成你想用的模型名
codewhale --provider ollama --model <model>
```

Codewhale 默认连接 `http://localhost:11434/v1`。想确认 Codewhale 能不能找到本机的 Ollama，运行 `codewhale doctor --probe-local`。

## 4. 换别的提供商或模型

- **换提供商**：再次打开 `/provider`（或 `F3`）。已经配过密钥的提供商，选中后回车就直接切换；没配过的，会带你走第 2 节的填密钥流程。也可以直接输入 `/provider <ID>`，例如 `/provider moonshot`。
- **换模型**：输入 `/model` 打开模型列表；或者直接输入 `/model <模型ID>`，例如 `/model deepseek-v4-pro`。
- **注意**：用 `/provider` 或 `/model` 切换，默认**只在本次会话有效**（用 `/model` 切换后，界面会提示「仅本次会话有效」）。想让下次启动也用它，再输入 `/model save-default`。

### 国内常见提供商（已内置）

下表只列一部分，全部内置提供商见 [PROVIDERS.md](./PROVIDERS.md)。「列表里的名称」是 `/provider` 列表中显示的英文名，按它找到对应的一项即可；「密钥环境变量」用于第 5 节的环境变量方式。

| 提供商 ID | 列表里的名称 | 密钥环境变量 |
|---|---|---|
| `deepseek` | DeepSeek（默认） | `DEEPSEEK_API_KEY` |
| `moonshot` | Moonshot/Kimi（月之暗面） | `MOONSHOT_API_KEY` 或 `KIMI_API_KEY` |
| `zai` | Zhipu AI / Z.ai（智谱） | `ZAI_API_KEY` 或 `ZHIPU_API_KEY` |
| `qianfan` | Baidu Qianfan（百度千帆） | `QIANFAN_API_KEY` |
| `volcengine` | Volcengine Ark（火山方舟） | `VOLCENGINE_API_KEY` 或 `ARK_API_KEY` |
| `siliconflow-CN` | SiliconFlow (China)（硅基流动中国区） | `SILICONFLOW_API_KEY` |
| `modelstudio-token-plan` | Alibaba Cloud Model Studio（阿里云百炼） | `MODELSTUDIO_API_KEY` 或 `DASHSCOPE_API_KEY` |
| `minimax` | MiniMax | `MINIMAX_API_KEY` |
| `stepfun` | StepFun / StepFlash（阶跃星辰） | `STEPFUN_API_KEY` |
| `xiaomi-mimo` | Xiaomi MiMo（小米） | `XIAOMI_MIMO_API_KEY` |
| `modelscope` | ModelScope（魔搭） | `MODELSCOPE_API_KEY` |
| `tencent` | Tencent TokenHub（腾讯） | `TENCENT_API_KEY` |

### 用任意「OpenAI 兼容」地址

如果你的平台不在内置列表里，但它说自己「兼容 OpenAI 接口」，可以这样接入：

1. 先把这个平台的密钥放进一个环境变量（方法见第 5 节），例如 `MY_GATEWAY_API_KEY`。
2. 打开 `/provider`，选 **Custom (OpenAI-compatible)**。
3. 依次填写：`Name`（自己起的名字，如 `my-gateway`）、基础 URL（平台文档给的地址）、默认模型（可选）、`API key env`（第 1 步的变量名，可选）。这个表单只保存变量名，不保存密钥本身。

也可以直接写进配置文件（`api_key_env` 填的是变量名，不是密钥）：

```toml
provider = "my-gateway"

[providers.my-gateway]
kind = "openai-compatible"
base_url = "https://gateway.example/v1"
api_key_env = "MY_GATEWAY_API_KEY"
model = "your-model-id"
```

如果平台本身已经在内置列表里，优先选内置的那一项。

## 5. 配置文件、密钥和环境变量

- **配置文件**：`~/.codewhale/config.toml`。Windows 上是 `C:\Users\你的用户名\.codewhale\config.toml`。
- **密钥存在哪**：在界面里填的、或用 `codewhale auth set` 保存的密钥，不写进 `config.toml`，而是放在本地密钥存储里，默认是 `~/.codewhale/secrets/` 下的一个私有文件；只有你明确选择时才改用系统钥匙串。
- **环境变量方式**：每个提供商都有自己的密钥环境变量（见上表）。在 macOS / Linux 上可以这样启动：

```bash
export DEEPSEEK_API_KEY="your-key"
codewhale
```

Windows 上在 PowerShell 里这样启动（只对当前这个窗口有效）：

```powershell
$env:DEEPSEEK_API_KEY = "your-key"
codewhale
```

想让以后打开的每个窗口都带上这个变量，运行一次 `setx DEEPSEEK_API_KEY "your-key"`，然后**关掉并重新打开**终端再启动 Codewhale；`setx` 不会影响已经打开的窗口。

- **谁说了算**：同一个提供商有多个密钥来源时，配置文件和密钥存储里的密钥**优先于**环境变量。所以只改环境变量、却发现还在用旧密钥时，看下一节第 1 条。
- **查看当前用的是哪个密钥**：`codewhale auth status --provider deepseek`。它只显示来源和末四位，不显示完整密钥。

## 6. 常见问题

**1. 提示密钥无效（例如 `Authentication Fails … is invalid`）**
密钥填错了或已被撤销。先运行 `codewhale auth status --provider deepseek`，看实际用的是哪个来源。重新保存用 `codewhale auth set --provider deepseek`；想改用环境变量里的密钥，用 `codewhale auth clear --provider deepseek` 清掉已保存的密钥。在界面里，无效密钥会把你带回选择提供商的界面，并在该提供商旁标出 `last check failed (authentication)`。

**2. 国内账号的密钥，连的却是国际站**
有些提供商的默认地址是国际站。例如 `moonshot` 默认是 `https://api.moonshot.ai/v1`；有用户报告，中国区 Kimi 密钥要配 `https://api.moonshot.cn/v1`，否则认证失败，而且改完 `base_url` 后要重新运行一次 `codewhale auth set --provider moonshot`。硅基流动国内账号请选 `siliconflow-CN`。完整的 Kimi 中国区实操见 [WINDOWS_BEGINNER.md](./WINDOWS_BEGINNER.md) 的 1.2.1 节。

**3. 地址填错或被拒绝**
基础 URL 要用 `https://`。非本机的 `http://` 地址会报 `Refusing insecure base URL`；`localhost`、`127.0.0.1` 这类本机地址可以用 `http://`。自定义提供商写了 `api_key_env`，但启动 Codewhale 的终端里没有这个变量（或为空），会直接报错，不会悄悄改用别的密钥。

**4. 模型列表里有，却用不了；或者第一次特别慢**
列表里有某个模型，不代表你的账户一定能调用它。每次都报同一个错时，换一个模型试试。有的提供商第一次调用还没加载的模型，要等 90 到 150 秒；不要急着判断「模型坏了」，可以调大 `[stream].open_timeout_secs`（默认 45 秒，最多 300 秒）。模型列表过旧时，运行 `codewhale models --update` 刷新。

**5. 网络连不上**
如果报 `Network error: SSE stream request failed …`，通常是连不到提供商的服务器。可以用 `curl -sI https://api.deepseek.com` 检查（返回 `401` 也正常，说明连得上）。如果你通过代理服务器上网，确认已经设置了 `HTTPS_PROXY`。也可以运行 `codewhale doctor --probe-api` 检查：连接失败时第一行是 `✗ API connection failed`（超时则是 `✗ API check got no answer in time`），下一行会给出具体原因，例如 `Invalid API key`（密钥无效）、`API key lacks permissions`（密钥没有权限）、`DNS resolution failed`（域名解析失败）或 `Connection failed`（连不上），按提示对照上面几条处理。

## 7. 下一步读什么

- [PROVIDERS.md](./PROVIDERS.md) —— 全部提供商的 ID、默认地址、默认模型和注意事项。
- [CONFIGURATION.md](./CONFIGURATION.md) —— 完整配置参考，包括密钥的查找顺序。
- [MODES.md](./MODES.md) —— Plan / Work 模式和权限姿态，开始让它改文件前建议先读。
- [GUIDE.md](./GUIDE.md) —— 用户指南：第一个任务、界面、命令和日常工作流。
