<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>模型可以是任何人的。智能体应当属于你。</h1>

Codewhale 是一个运行在终端里的开源编程智能体，用 Rust 编写。它支持任意模型，可以把一项任务分给多个智能体，而且它的每一行代码都任你阅读。

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

这会在 macOS 或 Linux 上安装最新发布版本，无需账号。[Windows、npm 和 Cargo](#安装) 的安装方式见下文。

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[网站](https://codewhale.net) · [文档](../README.md) · [更新日志](../../CHANGELOG.md) · [参与贡献](../../CONTRIBUTING.md)

[English](../../README.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>本翻译为机器草稿，尚未经母语者审校。</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="一个演示任务结束后的 Codewhale 终端界面。它报告已为 export.py 添加 --json 参数，3 个本地测试全部通过，委派的审查没有发现问题。下方的任务列表显示三个已完成的步骤，状态栏显示 Full Access 和本地 website-demo 提供商。">

<sub>尚未发布的 0.10.3 的开发构建，使用脚本化的测试模型，在一次性工作区中以 Full Access 运行。其中的编辑、三个测试和一次子智能体审查都是真实运行的。</sub>

</div>

## 安装

上面的命令会把经过校验和验证的 `codewhale` 及其简短别名 `codew` 放到 `~/.local/bin`。如果之后 shell 提示 `command not found`，请按安装器打印的 PATH 提示操作（[详情](../INSTALL.md#put-it-on-your-path)）。使用 `codewhale update` 升级。

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

预编译二进制文件覆盖 Linux、macOS 和 Windows 的 x64 与 arm64 架构。Scoop、Nix、Linux 上的 Homebrew、Android 和 Termux（预览版）以及手动下载见[安装指南](../INSTALL.md)，容器见 [Docker](../DOCKER.md)。每台机器只使用一种安装方式；否则 `PATH` 上会出现两份副本，排在前面的那一份生效。

## 快速开始

1. 提交或暂存你的改动，然后在 git 仓库根目录运行 `codewhale`。默认情况下，它会直接应用仓库内的文件修改而不询问，并在每条非只读的 shell 命令前先询问你。
2. 运行 `/provider`（或按 `F3`），选择一个提供商，粘贴它的 API 密钥，然后选择模型。默认提供商是 DeepSeek。首次启动且没有密钥时，Codewhale 会在你发送第一条消息时请你选择提供商；如果本地 Ollama 服务器有响应，它会提议使用本地模型。
3. 输入一个任务：

```text
Fix the failing tests and explain what changed.
```

只读命令会直接运行。其他任何 shell 命令，Codewhale 都会先询问：`y` 运行这一次，`a` 在本次对话接下来的时间里允许类似的命令，`n` 拒绝。`/undo` 会撤销你的上一个请求及其文件改动，`/help` 列出命令和按键。

在脚本和 CI 中，`codewhale exec` 不启动界面，只运行一个任务。用 `--auto` 给它工具之前，请先阅读[无界面运行](../INSTALL.md#headless-scripts-ci)，因为除非你加上沙箱，`--auto` 使用的是 Full Access。

## 功能

给 Codewhale 一个任务，它会阅读仓库、编辑文件并运行命令，直到任务完成或需要你的输入。[最新版本的发布说明](https://github.com/codewhale-hq/Codewhale/releases/latest)列出了最新改动，[更新日志](../../CHANGELOG.md)包含每个版本。

### 任意模型

内置超过 40 个提供商，包括 Anthropic、OpenAI、DeepSeek、Google、Mistral、Moonshot、OpenRouter 和 xAI。任何兼容 OpenAI 的端点都可以使用，通过 Ollama、vLLM 或 SGLang 运行的本地模型也可以。默认模型是 DeepSeek 的 `deepseek-flash`，它现在由 DeepSeek V4.1 Flash 提供（[详情](../PROVIDERS.md#deepseek-v41-flash)）。

`/model` 可在回合之间切换模型，`/model auto` 会为每个回合挑选一个模型（用 `/router` 设置挑选方式）。`/turn inspect` 显示某个回合使用了哪个模型、花费多少以及耗时多久。参见[提供商](../PROVIDERS.md)。

### 回执与撤销

`/receipts`（退出后可用 `codewhale receipts --last`）会列出一次会话修改过的文件、运行过的命令、发出的网页和 MCP 调用、启动的智能体、每次批准及批准人，以及失败的操作。`/undo` 一步撤销你的上一个请求及其修改的文件。`/restore` 会把整个工作区回滚到更早的快照；由于这会覆盖之后的编辑，它需要 `/trust on` 或 Full Access。参见[回执](../RECEIPTS.md)。

### 多个智能体共同完成一项任务

`/goal` 设置一个跨回合延续的目标。Codewhale 可以把工作分给[子智能体](../SUBAGENTS.md)，运行一组各有角色和模型的智能体 [Fleet](../FLEET.md)，或者按照提交在仓库中的[工作流](../WORKFLOW_AUTHORING.md)执行。

### 扩展

连接 [MCP 服务器](../MCP.md)，添加[技能](../SKILLS.md)和[插件](../PLUGINS.md)，并在会话和工具事件上运行[钩子](../HOOKS.md)。代码模式（实验性，默认开启）让模型编写一个调用多个 MCP 和插件工具的 `execute_tools` 程序；每次调用仍需遵守你的批准规则，并保留回执。v0.10.2 内置的 Computer Use 插件（实验性，仅限 macOS）可以观察并操作其他应用，在你审查其权限并启用之前一直处于关闭状态（[指南](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)）。即将在 0.10.3 推出：该插件将[单独发布](https://github.com/codewhale-hq/codewhale-cu-plugin)。

### 运行方式

| 命令 | 作用 |
| --- | --- |
| `codewhale` | 交互式终端应用 |
| `codewhale exec "…"` | 不启动界面运行一个任务，用于脚本和 CI（[无界面运行](../INSTALL.md#headless-scripts-ci)） |
| `codewhale web` | 运行在 `127.0.0.1` 上的本地网页客户端（[指南](../WEB.md)） |
| `codewhale review --pr N` | 对 GitHub 拉取请求进行建议性审查；[GitHub 机器人设置](../GITHUB_ACTION.md#set-up-your-own-bot)用你自己的模型密钥运行它 |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md)，一个本地 HTTP API，提供线程、事件、批准以及每个回合的文件改动 |
| `codewhale serve --acp` | 通过 stdio 提供 Agent Client Protocol，供 Zed 等编辑器使用 |

终端应用、`exec`、`web` 和 Runtime API 共用同一个引擎（[架构](../ARCHITECTURE.md)）。社区维护的 [VS Code 扩展](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)可以从侧边栏连接到它（[源代码](https://github.com/HengQuWorld/CodeWhale-VSCode)）。

## 如何保证安全

两个开关决定 Codewhale 可以自行做多少事。输入框为空时，`Tab` 切换模式：**Plan** 只阅读和搜索，不做任何修改；**Work** 编辑文件并运行命令；**Operate** 与 Work 权限相同，会一直工作到目标得到验证或需要你介入。`Shift+Tab` 切换权限。默认的 **Ask** 会直接应用仓库内的文件修改而不询问，并在非只读的 shell 命令前询问。**Auto-Review** 让规则和一个审查模型批准常规调用，并拒绝高风险调用；切换到 Ask 即可自己批准这些调用。**Full Access** 从不询问，并关闭 Codewhale 的沙箱，但仓库规则和托管策略仍然适用。[v0.10.2 指南](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md)有详细说明。

即将在 0.10.3 推出：`Shift+Tab` 将成为一个依次经过 Ask、Accept Edits、Plan、Auto-Review 和 Full Access 的拨盘，`Tab` 不再切换模式，Operate 也将取消。Ask 将在每次文件修改前也询问，而新的默认选项 Accept Edits 会应用 git 仓库内的修改，并仍在命令前询问。[当前指南](../MODES.md)对此有说明。

在 Ask 和 Auto-Review 中，命令会在系统提供的操作系统沙箱中运行：macOS 上是 Seatbelt，Linux 上安装了 bubblewrap 时是 bubblewrap。Windows 目前还没有操作系统沙箱，Full Access 会关闭沙箱。在沙箱内，shell 命令和终端会话无法读取 `~/.ssh`、`~/.aws` 和 `.env` 文件等凭据存储。`/preview-request` 会显示下一个请求的脱敏预览，而不会真正发送。

你不需要账号，用 `/provider` 添加的密钥只保存在你的机器上。使用你自己的 API 密钥时，你直接向提供商付费；本地模型不需要密钥。价格已知时，页脚和 `/cost` 会显示一次会话的花费；价格未知时显示为未知，绝不显示为免费。

遥测默认开启。它会发送聚合计数（版本和平台、会话时长和结果、你使用过的内置提供商、功能和错误计数），附带一个每 90 天轮换的随机安装 ID，绝不发送对话、代码、提示词、模型名称、文件或仓库名称以及凭据。用这条命令关闭它：

```bash
codewhale config set telemetry false
```

Codewhale 启动时还会在 GitHub 上检查新版本；设置 `CODEWHALE_NO_UPDATE_CHECK=1` 可以跳过。更多内容见[授权顺序](../AUTHORIZATION_ORDER.md)、[沙箱](../SANDBOX.md)和[遥测](../TELEMETRY.md)。

## 未来方向

Codewhale 正在成为横跨你的终端、桌面应用、浏览器和云端计算机的统一环境，全部由同一个引擎驱动。下面每个部分都说明了它今天所处的状态。

| 部分 | 状态 | 现状 |
| --- | --- | --- |
| 终端应用 | 已发布 | 在 macOS、Linux 或 Windows 上安装，然后在任意仓库中运行 `codewhale`。 |
| 本地浏览器客户端 | 已发布 | 运行 `codewhale web`，即可在浏览器中阅读对话、发送任务并回应审批；它随终端一起提供，无需账号。 |
| 多个智能体共同完成一项任务 | 已发布 | 子智能体、Fleet 和工作流让多个智能体各司其职，共同完成一项任务。 |
| 桌面应用 | 开发中，暂无下载 | 我们正在基于同一个引擎构建桌面应用，目前还没有公开下载。 |
| 云端计算机 | 仅限邀请 | 云端计算机仅限邀请，我们正在构建它们，让会话可以在云端的一台机器上持续工作。 |
| 跨越它们的同一个会话 | 建设中 | 我们正在构建同一个会话，它会随你从终端到桌面、浏览器和云端，并且自始至终都属于你。 |

[路线图](../ROADMAP.md)说明了每个版本涵盖的内容，以及你可以在哪里提供帮助。

## 从其他智能体迁移过来

Codewhale 会自动读取 `AGENTS.md`。为其他工具编写的指令文件，例如 `CLAUDE.md`、`.cursorrules` 或 `.github/copilot-instructions.md`，只有在你于配置中设置 `project_instruction_imports = ["claude"]`（或 `"cursor"`、`"cline"`、`"windsurf"`、`"copilot"`、`"all"`）选择启用后才会加载；参见 [config.example.toml](../../config.example.toml)。Codewhale 发现未加载的此类文件时会给出警告。

`/import-claude` 会准备一份 Claude Code 设置和 MCP 服务器的导入内容，供你审阅。`/plugin install` 接受包含技能、命令、智能体配置和 MCP 服务器的 Claude Code 插件，并拒绝需要钩子或 LSP 服务器的插件（[详情](../CLAUDE_PLUGIN_COMPAT.md)）。

## 文档

这些指南跟随主分支，可能会描述尚未包含在最新版本中的改动。

| 从这里开始 | 参考 |
| --- | --- |
| [用户指南](../GUIDE.md) | [配置](../CONFIGURATION.md) |
| [安装](../INSTALL.md) | [架构](../ARCHITECTURE.md) |
| [提供商与本地模型](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [模式与权限](../MODES.md) | [插件编写](../PLUGIN_AUTHORING.md) |
| [快捷键](../KEYBINDINGS.md) | [回执](../RECEIPTS.md) |
| [GitHub 拉取请求审查](../GITHUB_ACTION.md) | [全部文档](../README.md) |

## 参与贡献

Codewhale 由使用它的人公开构建，用许多种语言。

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale 贡献者墙：每一位在 Codewhale 中留下代码、共同署名提交、被延续的拉取请求或获得致谢的报告的人各有一个头像，顶部是总人数。"></a>

[贡献者墙](../CONTRIBUTORS.md)统计了每一位在 Codewhale 中留下代码、共同署名提交、被延续的拉取请求或获得致谢的报告的人，包括 GitHub 自己的贡献者列表漏掉的人。`scripts/contributor-wall.py` 会根据 git 历史和 GitHub 重新生成这张图和名单。

欢迎任何反馈：一个 bug、一个想法，或者某处看起来、用起来不对劲的地方。这些都可以[提交 issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose)；新功能也最好从 issue 开始。欢迎为修复、翻译、提供商和计划中的工作提交拉取请求。问题可以发到 [Discussions](https://github.com/codewhale-hq/Codewhale/discussions)。[贡献指南](../../CONTRIBUTING.md)说明了每种帮助该去哪里、你的工作如何署名合入，以及[各部分代码在哪里](../../CONTRIBUTING.md#project-structure)。

可以在 [Discord](https://discord.gg/37gfS3ksug) 上聊天，或者添加维护者 Hunter 的微信（`hunterbown`），申请加入 Whale Brothers 群。欢迎所有人。

## 历史与许可证

Codewhale 最初名为 `deepseek-tui`，至今仍能读取该项目的配置和会话。它由独立维护，与 DeepSeek 或任何其他模型提供商均无关联。

[MIT](../../LICENSE)。改编自其他开源项目的代码列在第三方声明中：[内置的源代码](../../THIRD_PARTY_NOTICES.md)和[借鉴的设计](../THIRD_PARTY_NOTICES.md)。
