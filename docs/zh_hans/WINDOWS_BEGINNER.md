# Codewhale Beginner Guide for Windows (简体中文)

> 本文面向**完全没接触过 AI 编程智能体、使用 Windows 系统**的初学者。命令和路径按当前实现整理；本次文档更新未重新进行 Windows 实机验证。
>
> 本文为中文原创文档（无对应英文版），2026-08-11 首发，2026-09-29 依据当前代码与英文文档复核并更新。

---

## 0. 一页速览

| 问题 | 一句话答案 |
|---|---|
| Codewhale 是什么？ | 装在你自己电脑上的"编程智能体"，能读文件、改代码、跑命令、自己验证结果 |
| 要花钱吗？ | 软件开源免费，但模型要你自己带 API key（默认 DeepSeek） |
| 在哪干活？ | 启动文件夹是工作区；具体读写范围由权限与工具检查决定。Windows 当前不提供 OS 级命令沙箱 |
| 第一步做什么？ | 装好后建一个空文件夹 → `cd` 进去 → 运行 `codewhale` |
| 会不会乱动我的文件？ | 默认 Ask 会为需要审批的工具调用提问；工作区内的普通文件编辑可以直接执行并显示 diff，先提交或备份重要改动 |
| 做小工具要用哪个模式？ | Plan（先出方案）+ Work（再动手），Ask 权限，全程足够 |
| 用哪个模型？ | `auto` 是路由策略，不是模型：默认每回合仍用你设定的默认模型；只有你配置了路由器或 `cost_saving` 才会自动换模型（见 7.2 节） |
| 对话能导出吗？ | 能：`/export file 文件名.md` |

**操作问题速查**

| 操作问题 | 解决办法 |
|---|---|
| 双击运行提示找不到 VCRUNTIME140_1.dll | 装 VC++ 运行库（见 2.4 节） |
| 终端里输入 codewhale 提示"不是命令" | 环境变量没配好或终端没重开（见 2.3 节） |
| 提示"禁止运行脚本" | 先检查 PowerShell 的有效执行策略（见第 3 节） |
| 配置了 pro 却显示在用 flash | 多半是用了 `/model auto` 并开启了 `[auto] cost_saving`，或配置了 `[auto.router]`，不是 bug（见 7.2 节） |
| 界面停在奇怪的模式 | 按 `Shift+Tab` 切回，或输入 `/mode work` |
| 改完配置突然连不上 | 检查 `provider` 和 `base_url` 是否被改坏，改回默认 |
| cmd 里运行显示异常或崩溃 | 改用 Windows Terminal（见 2.5 节） |
| GitHub 打不开/下载失败 | 需要配置系统代理后重试 |

---

## 1. 先理解 3 个核心概念

### 1.1 它不是聊天机器人，是"能干活的智能体"

- **聊天机器人**：只回答你的问题，不动你电脑上的任何东西
- **Codewhale**：给它一个任务，它能读你的文件、修改代码、在终端里运行命令、自己检查结果，做完或需要你拍板时才停下

它运行在**你自己的电脑**上（开源，项目地址 `https://github.com/codewhale-hq/CodeWhale`），所以它能动你的真实文件。

### 1.2 模型由你自己带（自带 API key）

Codewhale 不是开箱即用的服务，你需要一个模型提供商的 API key。**默认是 DeepSeek**，设置命令：

```powershell
codewhale auth set --provider deepseek
```

除 DeepSeek 外，Codewhale 还内置支持下面这些**常用提供商**（`provider` 后面的英文 ID 是配置和命令行里要用的名字，来自官方文档）。提供商列表更新很快，完整且最新的清单以 [PROVIDERS.md](PROVIDERS.md) 为准：

**面向中国大陆的提供商示例（可达性以实际网络和账户为准）**

| Provider ID | 提供商 | 说明 |
|---|---|---|
| `deepseek` | 深度求索 DeepSeek | 默认提供商，V4 pro / flash |
| `moonshot` | 月之暗面 | Kimi 系列 |
| `zai` | 智谱 Z.ai | GLM 系列 |
| `stepfun` | 阶跃星辰 | Step 系列 |
| `minimax` | MiniMax | MiniMax 系列 |
| `qianfan` | 百度千帆 | 百度大模型 |
| `wanjie-ark` | 万界 Ark | OpenAI 兼容托管 |
| `volcengine` | 火山引擎（字节跳动） | ARK 平台 |
| `xiaomi-mimo` | 小米 | MiMo 系列 |
| `siliconflow` | 硅基流动 | 模型聚合平台 |
| `siliconflow-CN` | 硅基流动（中国区） | 国内域名 |
| `modelscope` | 魔搭社区 | 大模型开源社区 |
| `longcat` | 美团 | LongCat 系列 |
| `telecomjs` | 中国电信 | 天翼 AI 网关 |
| `csdn` | CSDN | OpenAI 兼容接口 |
| `modelstudio-token-plan` / `modelstudio-coding-plan` | 阿里云百炼（Model Studio） | Token Plan / Coding Plan 套餐；对应的 `-anthropic` ID 走 Anthropic 消息协议 |

**国际提供商（可能需要代理访问）**

| Provider ID | 提供商 | 说明 |
|---|---|---|
| `openai` | OpenAI | GPT 系列；也可用于任何 OpenAI 兼容网关 |
| `anthropic` | Anthropic | Claude 系列 |
| `xai` | xAI | Grok 系列 |
| `meta` | Meta | Llama 系列 |
| `nvidia-nim` | NVIDIA | NIM 托管推理 |
| `openrouter` | OpenRouter | 聚合中转，一个 key 用多家模型 |
| `novita` | Novita AI | OpenAI 兼容托管 |
| `fireworks` | Fireworks AI | 推理平台 |
| `together` | Together AI | 推理平台 |
| `arcee` | Arcee AI | Trinity 系列 |
| `deepinfra` | DeepInfra | 推理平台 |
| `huggingface` | Hugging Face | Inference Providers |
| `openmodel` | OpenModel | 托管推理 |
| `atlascloud` | AtlasCloud | OpenAI 兼容托管 |
| `sakana` | Sakana AI | Fugu 系列 |
| `openai-codex` | OpenAI Codex | Codex 编程模型 |
| `opencode-go` / `opencode-zen` | OpenCode | Go 订阅通道 / Zen 模型网关 |
| `ollama-cloud` | Ollama Cloud | Ollama 托管推理（需要 API key） |
| `mistral` | Mistral AI | Mistral 系列 |
| `google` | Google | Gemini 系列（官方 OpenAI 兼容接口） |
| `edenai` | Eden AI | 模型聚合平台 |
| `zenmux` | ZenMux | 模型聚合平台 |
| `concentrate` | Concentrate | 聚合网关（OpenAI Responses 协议） |
| `codewhale` | Codewhale | 使用 Codewhale 账户，按账户的模型目录选择协议 |

**本地/自建（是否需要 API key 取决于服务配置）**

| Provider ID | 说明 |
|---|---|
| `ollama` | 本地跑开源模型（如 codewhale-coder） |
| `sglang` | 自建推理服务（localhost:30000） |
| `vllm` | 自建推理服务（localhost:8000） |

**特殊通道（普通用户无需理会，保持默认即可）**

| Provider ID | 说明 |
|---|---|
| `deepseek-anthropic` | DeepSeek 走 Anthropic 消息协议（给只认 Claude 格式的工具用，模型和 API key 都不变） |
| `minimax-anthropic` | MiniMax 走 Anthropic 消息协议（同上） |

> 切换提供商：界面里用 `/provider` 命令选，或改配置文件 `provider = "提供商ID"`。国内用户最常组合：`deepseek`（省钱）、`moonshot`/`zai`（备选）、`ollama`（本地免费）。

### 1.2.1 实战案例：切换到 Kimi（moonshot）中国区

以切换 Kimi（月之暗面）中国区为例，完整走一遍"换提供商"的流程（本案例经过实际验证）。

**前置：先去拿 key**

从中国区开放平台拿 API key：`https://platform.kimi.com`（注意是中国区域名，不是 `platform.kimi.ai` 国际站）。**中国区、国际站的 API key 不通用，请勿混用。**

**第一步：改配置文件，指定中国区地址**

编辑 `C:\Users\你的用户名\.codewhale\config.toml`，在文件里加入（或找到）`[providers.moonshot]` 段：

```toml
[providers.moonshot]
auth_mode = "api_key"
base_url = "https://api.moonshot.cn/v1"
```

> 关键点：**中国区 key 必须配中国区地址 `api.moonshot.cn/v1`**。如果保持默认（国际站 `api.moonshot.ai/v1`），用中国区 key 会报 `Invalid Authentication`（认证失败）。

**第二步：重新设置密钥（实测必需，只改 base_url 不够）**

在 PowerShell 里执行：

```powershell
codewhale auth set --provider moonshot --api-key "你的中国区Kimi API key"
```

> 实测发现：添加 base_url 后，**必须重新输入一遍密钥**才能真正生效。

**第三步：在 Codewhale 里切换**

1. 输入 `/provider` 打开提供商选择器 → 选 **moonshot**（可能显示 missing key，不用管，继续选）
2. 如果提示输入 key，就粘贴你的中国区 key
3. 选模型：`/model kimi-k3`（最强，1M 上下文）或 `/model kimi-k2.7-code`（默认稳定）
4. 发一条测试消息，能正常回复就说明切换成功
5. 输入 `/status` 确认 provider 是 moonshot、地址是 `api.moonshot.cn/v1`

**Kimi 中国区可用模型**

| 模型 ID | 说明 |
|---|---|
| `kimi-k3` | 最强，永远思考，1M 上下文；思考强度选 `off` 会被自动当作 `low` |
| `kimi-k2.7-code` | 编程版，默认稳定，推荐先用这个 |
| `kimi-k2.6` | 更轻量 |

> 注："k3"是 Kimi Code 平台对 `kimi-k3` 的简写，本文档使用全称 `kimi-k3`。

**注意事项**

- **不要用 `/model auto`**：Kimi 没有便宜的"flash 档"，auto 只会落在默认模型上，直接固定模型更实在
- **deepseek 不受影响**：切回用 `/provider deepseek` + `/model deepseek-v4-pro`，随时可换
- **Kimi Code 会员模型别混用**："k3"（即 `kimi-k3` 的简写）/ `kimi-for-coding` 是 Kimi Code 会员平台专属模型（入口 `api.kimi.com/coding/v1`），与普通 API 入口的 `kimi-k3` 不同，普通中国区 API key 不要用这些 ID
- **改动 base_url 后要重启 Codewhale** 才生效

### 1.3 工作区（workspace）概念

**在哪个目录启动，它就操作哪个目录。**

- 终端里 `cd` 到你的项目文件夹，再运行 `codewhale`
- 它只在这个目录里干活，目录外访问需要额外信任
- **做小工具的第一步：先建一个空文件夹**，比如 `C:\Users\你的用户名\Desktop\my-tools`

---

## 2. Windows 安装步骤（共 5 步）

### 2.1 第一步：下载安装包

打开官方发布页：

```
https://github.com/codewhale-hq/CodeWhale/releases
```

在最新版本的文件列表中，普通 Windows 电脑（Intel/AMD 处理器）**推荐下载 `codewhale-windows-x64-portable.zip`（Windows 便携版）**。

- **便携版 = 解压即用，不需要安装脚本**：不用装 npm、Scoop、Cargo，也不用双击安装程序
- 下载后解压到一个文件夹，里面就是可直接运行的程序（本说明按当前 zip 内容列出）：
  - `codewhale.exe` — 主程序
  - `codew.exe` — 同一二进制的短命令名
  - `codewhale.bat` — 启动器：已安装 Windows Terminal 时用它打开，否则回退到直接运行 exe

文件名带 **arm64** 的是给 ARM 架构设备用的，普通电脑不要选。如果你更习惯传统安装方式，也可以下载 **`CodeWhaleSetup.exe`**（Windows 安装器）：它会安装到 `%LOCALAPPDATA%\Programs\CodeWhale\bin` 并自动加入用户 PATH，开始菜单快捷方式指向 `codewhale.bat`，无需手动配置环境变量；因为安装包未签名，双击会弹 Windows SmartScreen 提示，点"更多信息 → 仍要运行"即可。注意：发布页里的 `codewhale-windows-x64.exe` 是**纯命令行程序，不是安装器**，双击只会打开默认 cmd 窗口，请改用 zip 里的 `codewhale.bat` 或安装器的开始菜单项。

![GitHub 发布页，选择 windows-x64 版本](../images/github-release-page.png)

### 2.2 第二步：放到固定目录【codewhale-windows-x64-portable.zip】

把便携版 zip 解压后的文件夹放到一个固定位置，比如 `D:\codewhale`（解压后里面就是 `codewhale.exe`、`codew.exe`、`codewhale.bat`，完整路径为 `D:\codewhale\codewhale.exe`）。放好后**不要再移动它**，否则下面的环境变量会失效。从资源管理器启动时请双击 `codewhale.bat`，不要双击 `codewhale.exe`。

> 升级方法：以后出新版本，在终端运行 `codewhale update` 即可（想先看有没有新版：`codewhale update --check`），它会自动下载、校验并替换程序文件，完成后重启 Codewhale。配置和对话记录都保留。网络受限时也可以下载新版 portable zip 解压覆盖同目录下的程序文件。

### 2.3 第三步：加入环境变量【codewhale-windows-x64-portable.zip】

加了环境变量，才能在任何文件夹里直接输入 `codewhale` 启动它。

1. 打开 Windows【设置】→【系统】→【系统信息】，点击右侧的【高级系统设置】

![打开高级系统设置](../images/windows-system-info.png)

2. 在弹出的【系统属性】窗口点【环境变量(N)…】
3. 在"用户变量"里找到 **Path**，点【编辑(E)…】→【新建(N)】，填入程序所在目录 `D:\codewhale`，一路点【确定】保存

![把 D:\codewhale 加入 Path 环境变量](../images/env-path-setting.png)

> 注意：改完环境变量后，**已经打开的终端窗口要关掉重开**才会生效。

### 2.4 第四步：安装运行库（解决 dll 报错）

第一次双击运行如果弹出"**由于找不到 VCRUNTIME140_1.dll，无法继续执行代码**"，说明系统缺少运行库，安装微软官方运行库即可：

1. 下载地址：`https://aka.ms/vs/17/release/vc_redist.x64.exe`（64 位系统选 x64，32 位选 x86）
2. 双击安装，完成后重新启动 `codewhale.exe`

![VCRUNTIME140_1.dll 报错及解决办法](../images/vc-runtime-dll-error.png)

> 离线/内网备选：从其他已装该运行库的电脑复制 `C:\Windows\System32\VCRUNTIME140_1.dll` 到本机同目录，或放到 `codewhale.exe` 同级目录下。

### 2.5 第五步：安装 Windows Terminal（不要用 cmd）

Codewhale 需要在终端里运行。**建议使用 Windows Terminal（即"终端"），不要用系统自带的 cmd（命令提示符）**。Codewhale 是 TUI（终端用户界面）软件，依赖终端渲染能力——cmd 功能有限，可能出现显示异常或崩溃；Windows Terminal 支持更多颜色、Unicode 字符和 GPU 渲染，运行 Codewhale 更稳定流畅：

```
https://learn.microsoft.com/zh-cn/windows/terminal/install
```

装好后打开 Windows Terminal，输入 `codewhale` 回车，能进入界面就说明安装成功。

---

## 3. 首次启动设置

首次启动只询问本次安装仍缺少的决定：无法推断语言时选择语言，没有可用路由时配置提供商（也可选择离线路由），当前文件夹需要信任决定时确认工作区信任。就绪后进入编辑器；命令行中传入的任务会保留，也可以从当前文件夹的任务建议开始。

之后可用 `/setup` 重新打开设置，用 `/constitution` 管理宪章。

**Windows 常见问题：PowerShell 执行策略**

运行脚本时若出现“禁止运行脚本”，先查看当前的有效策略及各作用域：

```powershell
Get-ExecutionPolicy
Get-ExecutionPolicy -List
```

在你自己管理的电脑上，如果确认需要允许运行本地脚本，可将当前用户的策略设为 `RemoteSigned`：

```powershell
Set-ExecutionPolicy -Scope CurrentUser RemoteSigned
```

这会持久更改当前用户的设置，直到再次修改；它不保证所有脚本都能运行。`RemoteSigned` 仍限制来自互联网的未签名脚本，组织的组策略也可以覆盖用户设置。具体行为和恢复方式见 [Microsoft 的执行策略说明](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_execution_policies)。

---

## 4. 给新手的"做小工具"最短路径

1. 建一个文件夹，比如 `C:\Users\你的用户名\Desktop\my-tools`
2. 在 Windows Terminal 里 `cd` 进去，运行 `codewhale`
3. 保持 **Plan + Ask**，发一条："我是新手，想做一个 XX 小工具。先帮我看看这个目录，然后给我一个实现方案，先不要改任何文件。"
4. 方案满意后，说"按方案实现，每步改动前告诉我"——它会开始写代码，你逐个批准
5. 让它"运行并验证给我看"，确认可用后，小工具就做好了

**新手安全口诀：先 Plan 出方案 → 切 Work 动手 → 全程保持 Ask。**

---

## 5. 两种模式：Plan / Work

按 `Shift+Tab` 在权限拨盘上逐档切换（Ask → Accept Edits → Plan → Auto-Review → Full Access），Plan 档就是 Plan 模式，其余每一档都是 Work；也可以输入 `/mode plan|work` 直接切换（`act` 仍是 Work 的兼容别名）。`Tab` 只做补全，不切换模式。

### 5.1 Plan 模式——只读，先出方案

- 只能看文件和设计，**不能改文件、不能跑命令**
- 用途：先让它"出方案给你看"，满意再动手
- **新手做小工具的第一步：先切到 Plan 让它出方案**

### 5.2 Work 模式——动手干活

- 普通的多步执行模式，能改文件、跑命令；是否弹窗问你，取决于第 6 节的权限姿态（默认 Accept Edits：仓库内编辑直接执行，命令会先问）
- 批准一次调用不等于绕过后续的工具路径检查或其他安全限制
- **新手第二步：方案满意后切到 Work，让它逐项实现**

> Operate 模式已退役：旧配置里的 `operate` 会按 Work 加载并提示一次。需要多个智能体并行干活时用 `/fleet`（舰队）。

> 模式会被记住：切过的模式写进配置，下次启动默认还是它。如果发现界面"停在奇怪的模式"，按 `Shift+Tab` 切回或输入 `/mode work`。

---

## 6. 权限与安全（小白最重要的保护）

按 `Shift+Tab` 在拨盘上切换四种权限姿态（permission posture），Plan 档位于 Accept Edits 与 Auto-Review 之间：

| 姿态 | 行为 | 建议 |
|---|---|---|
| Ask | 每次文件编辑和需要审批的调用都会先询问 | 想逐项确认时用 |
| **Accept Edits**（接受编辑） | git 仓库内的普通文件编辑直接执行；命令仍会先询问 | 默认档位；先保留重要文件的备份 |
| Auto-Review | 自动审查工具调用；确定性规则判定安全的操作直接放行，其余可审查的拦截交给一次性模型复核，高风险仍会被拦下。模型仍可为需要你决定的事项提问 | 熟悉后可用 |
| Full Access（完全访问） | 工具不经询问直接运行，命令可以访问这台电脑上的任何内容 | 只用于完全信任的文件夹 |

---

## 7. 模型与思考强度（最多人困惑的部分）

### 7.1 模型：pro 和 flash 是什么

- `deepseek-v4-pro`：**强模型**。思考更深入、代码更稳，但更慢更贵
- `deepseek-v4-flash`：**快模型**。便宜、响应快，适合简单任务

### 7.2 "auto" 不是模型，是路由策略

你可能发现：配置里明明写的是 pro，会话里却在用 flash。原因是 **`auto` 路由**：

- 用 `/model auto`（或 `model = "auto"`）时，每回合默认仍用你**声明的默认模型**。Codewhale 不会再根据请求的措辞或长度去猜"该用便宜还是强的模型"（旧的关键词/长度启发式已经移除，也没有默认的分类器）
- 只有你自己配置了下面两项之一，auto 才会换模型：
  - `[auto.router]`：你指定一个分类器（提供商 + 模型），由它逐回合挑选模型；可用 `/router` 设置
  - `[auto] cost_saving = true`：优先选用当前提供商的快速版本（如 flash）
- 会话顶部显示的 `Auto model route: deepseek-v4-flash` 就是 auto 的结果；用 `/status` 可以看到这一回合走的是哪条路径。这**不是 bug**

### 7.3 怎么切换模型

临时切换（只影响当前会话，推荐先用这个）——在底部输入框敲：

```
/model deepseek-v4-pro     固定用 pro（最强）
/model deepseek-v4-flash   固定用 flash（最快）
/model auto                改用 auto 路由（见 7.2 节）
```

只输入 `/model` 会打开选择器，上下键选模型回车确认。

永久改默认（影响以后所有新会话）——编辑配置文件：

```toml
default_text_model = "deepseek-v4-pro"
```

### 7.4 思考强度：max / high / low / off

这是另一个独立旋钮：**模型决定"谁在回答"，思考强度决定"回答前想多深"**。

| 档位 | 含义 | 适合场景 |
|---|---|---|
| off | 不思考，直接给结果 | 最快最便宜，简单查询 |
| low / medium / high | 思考深度递增 | 常规任务 |
| max | 思考到最深 | 架构设计、疑难 bug |
| auto | 系统每回合自动挑 | 默认推荐 |

- 完整取值还包括 `minimal`、`xhigh`、`ultra` 等，不同提供商的支持范围不同，以 [CONFIGURATION.md](CONFIGURATION.md) 为准
- 切换：键盘 **`Ctrl+T`** 循环切换，或在 `/model` 选择器里选
- 最强组合 = `deepseek-v4-pro` + `max`；最省组合 = flash + off（产品内部叫 "Fin" 路径）

> 注意：旧版配置模板注释里写过 "Shift+Tab 循环推理强度"，但现在 `Shift+Tab` 循环的是**权限姿态**，思考强度由 `Ctrl+T` 负责。

---

## 8. 配置文件在哪

- 全局配置：`C:\Users\你的用户名\.codewhale\config.toml`
- 模式、模型、思考强度等交互式选择会另外保存到 `~/.codewhale/settings.toml`（Windows 上位于 `C:\Users\你的用户名\.codewhale\`）
- 界面编辑：输入 `/config` 打开配置编辑器
- 查看哪些配置能改、能保存：`/config audit`

高频配置项：

| 配置键 | 含义 | 新手建议 |
|---|---|---|
| `default_text_model` | 默认模型（pro/flash） | 保持默认或 auto |
| `provider` | API 提供商 | 保持默认 |
| `reasoning_effort` | 思考强度 | 保持 `auto` |
| `[projects."路径"] trust_level` | 项目信任标记 | **不认识的文件夹别标 trusted** |
| `approval_policy` | 审批策略（`on-request` / `untrusted` / `never`） | 新手保持默认 `on-request`，想更严格可用 `untrusted` |
| `base_url`（写在 `[providers.xxx]` 下） | API 地址 | **别乱改**，改错会连不上 API |

---

## 9. 常用命令与快捷键速查

### 斜杠命令（输入 `/` 可看到全部）

| 命令 | 作用 |
|---|---|
| `/model` | 切换模型/思考强度（如 `/model deepseek-v4-pro`、`/model auto`） |
| `/provider` | 切换 API 提供商 |
| `/mode` | 切换模式（plan/work/operate） |
| `/config` | 编辑配置（`/config audit` 查看可编辑项） |
| `/setup` | 重新打开首次设置流程 |
| `/compact` | 对话太长时压缩上下文、省 token |
| `/review` | 让 AI 对代码做结构化审查 |
| `/skills` | 打开 Extensions → Skills；`/skills manage` 打开技能管理器 |
| `/status` | 查看当前模型/路由等状态 |
| `/export` | 导出对话 |
| `/constitution` | 管理宪章（高级） |

### 快捷键

| 按键 | 作用 |
|---|---|
| `Tab` | 补全命令和文件提及 |
| `Shift+Tab` | 权限拨盘 Ask → Accept Edits → Plan → Auto-Review → Full Access |
| `Ctrl+T` | 循环思考强度 |
| `Ctrl+Alt+O` | 打开 Turn Inspector（查看每回合用了哪个模型、为什么） |

---

## 10. 导出对话（/export）

把当前对话导出为 Markdown、HTML 或 JSON 文件：

```
/export file 对话记录.md
```

写相对路径时，文件会生成在你**当前工作目录**（启动 Codewhale 的文件夹）下。不写路径（`/export file`）时会自动命名，保存到 Codewhale 主目录的 `exports` 文件夹，并在回执里告诉你完整路径。

### 基本格式

```
/export [clipboard|file [路径]|turn [clipboard|file [路径]]] [--format md|html|json] [--no-tools] [--force]
```

### 完整命令表

| 命令 | 作用 |
|---|---|
| `/export` | 整个对话复制到剪贴板（默认行为，不带参数就是这个） |
| `/export clipboard` | 同上，显式写法 |
| `/export file 文件名.md` | 整个对话导出为 Markdown 文件 |
| `/export file 文件名.html` | 导出为单文件 HTML 页面（自带明暗两套配色，不含脚本） |
| `/export file 文件名.json` | 导出为带类型标记的结构化 JSON |
| `/export file` | 不写路径，自动命名并保存到 Codewhale 主目录的 `exports` 文件夹 |
| `/export --format html` | 同上，格式为 HTML（`--format json` 同理） |
| `/export file 文件名.md --no-tools` | 不包含工具调用和工具结果 |
| `/export file 文件名.md --force` | 文件已存在时强制覆盖（不加 `--force` 会拒绝覆盖） |
| `/export turn` | 只导出当前这一回合（handoff）到剪贴板 |
| `/export turn file 文件名.md` | 只导出当前这一回合为 md 文件 |
| `/export turn file --force 文件名.md` | 当前这一回合导出并强制覆盖 |
| `/daochu` | `/export` 的中文别名，完全等价 |

> 兼容旧写法：`/export 路径.md` 和 `/export turn 路径.md` 也可以直接用（等价于带 `file` 的写法）。`--force`、`--no-tools`、`--format` 可以放在命令里的任意位置。
>
> 文件格式默认由扩展名决定（`.md`、`.html`、`.json`），也可以用 `--format` 指定。HTML 和 JSON 只能写入文件，不能复制到剪贴板；`--no-tools` 和 `--format` 只用于整个对话，不用于 `turn`。路径里有空格时请用引号括起来。

### 导出的文件长什么样

开头是元信息，然后是你的全部对话：

```markdown
# Codewhale conversation export

- Exported: 2026-08-02T...   （导出时间）
- Title: ...                 （会话标题，没有标题时不出现）
- Session: xxxx              （会话 ID）
- Provider: moonshot         （当前提供商）
- Model: kimi-k3             （当前模型）
- Mode: agent                （当前模式；Work 模式在内部记作 agent）
- Workspace: superpower      （工作区名）
- Messages: N                （消息条数）
```

### 注意事项

- 只含对话内容，**AI 的内部推理过程和系统提示词被省略**，只有最终回复；工具调用和结果默认包含（可用 `--no-tools` 去掉），工具输出放在代码围栏里
- 类密钥内容（如 API key）和带凭据的 URL **自动打码**，分享前仍建议检查一遍
- 导出是只读操作，不影响当前会话
- 想省 token 而不是导出时，用 `/compact` 压缩上下文（见第 9 节）
