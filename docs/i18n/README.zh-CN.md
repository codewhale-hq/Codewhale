<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**在终端中运行、可搭配任意模型的开源编程智能体。**

给 Codewhale 一个任务，它会读取仓库、编辑文件、运行命令，直到任务完成或需要你提供输入为止。
它支持超过 40 个内置提供商（默认使用 DeepSeek V4.1 Flash），也支持在你自己机器上运行的模型。
`/receipts` 会列出它修改过的文件、运行过的命令，以及它发起的 Web 和 MCP 调用。

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[官网](https://codewhale.net) · [文档](../README.md) · [更新日志](../../CHANGELOG.md) · [参与贡献](../../CONTRIBUTING.md)

[English](../../README.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="一次 Codewhale 终端会话" width="760">

<sub>一个使用脚本化测试模型的 0.10.2 开发版本，在一次性工作区中以“完全访问”权限运行。其中的编辑、三个测试和一次子智能体审查都是真实运行的。</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter：DeepSeek v4.1 Flash 用量增加 30%" width="760">
  </a>
</p>

[查看 Codewhale × OrcaRouter 优惠](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale 会从 OrcaRouter 获得推荐佣金。

## 安装

macOS 和 Linux：

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

安装器会把经过校验和验证的 `codewhale` 及其简短别名 `codew` 放到 `~/.local/bin`。
如果之后 shell 提示 `command not found`，请运行安装器打印出的那行 PATH 命令
（[详情](../INSTALL.md#put-it-on-your-path)）。使用 `codewhale update` 升级。这些命令安装的是最新发布版本。
本页跟随 `main` 更新，因此标注了版本号的功能会在该版本中推出；[更新日志](../../CHANGELOG.md)列出了每个版本新增的内容。

<details>
<summary><b>Windows、npm、Cargo 及其他安装方式</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

预编译二进制文件覆盖 Linux、macOS 和 Windows 的 x64 与 arm64 架构。Nix、Linux 上的
Homebrew、Android/Termux（预览版）以及手动下载见[安装指南](../INSTALL.md)，容器见
[Docker](../DOCKER.md)。每台机器只使用一种安装方式；否则 `PATH` 上会出现两份副本，
排在前面的那一份生效。

</details>

## 快速开始

1. 先提交或 stash 你的改动，然后在 git 仓库的根目录运行 `codewhale`。默认情况下，
   它会直接在这里应用文件编辑而不询问，并向你展示 diff。
2. 运行 `/provider`（或按 `F3`），选择一个提供商，粘贴它的 API 密钥，然后选择模型。
   默认提供商是 DeepSeek。没有密钥时，可以改选本地运行时。如果尚未配置任何提供商，
   而 Ollama 已经在提供聊天模型，Codewhale 会自动使用它；
   从 0.10.2 起，它会在你发送第一条消息时提议使用它。
3. 输入一个任务：

```text
Fix the failing tests and explain what changed.
```

只读命令会直接运行。其他任何 shell 命令，Codewhale 都会先询问：`y` 运行这一次，
`a` 在本次对话接下来的时间里允许类似的命令，`n` 拒绝。`/undo` 会撤销你的上一个请求及其
文件改动。`/help` 列出命令和按键。

在脚本和 CI 中，`codewhale exec` 不启动界面，只运行一个任务。默认情况下，`--auto` 使用
“完全访问”权限：工具不经询问就运行，shell 命令不受 Codewhale 沙箱限制，信任模式允许
文件工具写入工作区之外的位置。加上 `--sandbox workspace-write` 可保留 shell 沙箱和
文件工具的工作区限制，并关闭信任模式：

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## 模式与权限

全新安装后，默认处于 Work 模式和“询问”权限。

| | 切换方式 | 选项 |
| --- | --- | --- |
| 模式 | `Tab`（输入框为空时）或 `/mode` | **Plan** 只读取和搜索，不做任何改动。**Work** 编辑文件并运行命令。**Operate** 拥有与 Work 相同的访问权限，把你的请求当作一个目标，并持续推进，直到目标得到验证或需要你提供输入为止。 |
| 权限 | `Shift+Tab` | **询问**（Ask）在你的仓库中直接应用文件编辑而不询问，并在运行非只读的 shell 命令之前先询问。**自动审核**（Auto-Review）让规则和一个审查模型批准常规调用，并拒绝高风险调用；如需亲自批准这些调用，请切换到“询问”。**完全访问**（Full Access）从不询问，并关闭 Codewhale 的沙箱；仓库规则和受管策略仍然适用。 |

详情见[模式与权限指南](../MODES.md)。

## 功能

### 0.10.2 新增内容

- `/pet on` 会把你的终端变成一头动画鲸鱼，展示智能体正在做什么，同时消息框仍可正常使用。
  它的 980 个粒子会用英文、中文或日文拼出当前步骤。
- `/workbar terminal` 会实时显示智能体正在运行的 shell。你可以在其中输入，
  也可以按 `Ctrl+N` 打开一个你自己的 shell（macOS 和 Linux）。
- `/undo` 一步撤销你的整个上一个请求，包括它修改过的每个文件；`/diff` 也可以在没有 Git
  的文件夹中使用。
- 当计划准备就绪时，Codewhale 会询问你是以“询问”还是“自动审核”权限开始
  Work，还是继续规划。
- `/color` 为每个对话设置各自的输入框颜色，并随会话一起保存。
- `web_search` 可以使用 DeepSeek 新推出的 Search API：在拥有 DeepSeek 密钥的情况下设置
  `[search] provider = "deepseek"`。
- 在“完全访问”之外，会上传凭据的 `curl` 或 `wget` 命令未经你批准绝不会运行，
  隐藏了不可见 Unicode 字符的命令会被拒绝。`write` 不再替换智能体尚未读取过的文件，
  也不再替换在它读取之后发生过变化的文件。

### 模型

内置超过 40 个提供商，包括 Anthropic、OpenAI、DeepSeek、Google、Mistral、Moonshot、
OpenRouter 和 xAI。任何兼容 OpenAI 的端点都可以使用，通过 Ollama、vLLM 或 SGLang
运行的本地模型也可以。默认模型是 DeepSeek 的 `deepseek-flash`，DeepSeek 已于 2026-10-17
将其切换为 DeepSeek-V4.1-Flash-1017；0.10.1 无需更新即可运行它
（[详情](../PROVIDERS.md#deepseek-v41-flash)）。

`/model` 可在回合之间切换模型，`/model auto` 会为每个回合挑选一个模型（用 `/router`
设置挑选方式）。`/turn inspect` 显示某个回合使用了哪个模型、花费多少以及耗时多久。
`codewhale auth chatgpt` 让你可以用 ChatGPT 套餐代替 API 密钥（实验性）。
参见[提供商](../PROVIDERS.md)。

### 回执与撤销

`/receipts`（退出后可用 `codewhale receipts --last`）会列出修改过的文件、运行过的命令、
Web 和 MCP 调用、启动过的智能体、审批及审批人，以及失败记录。`/undo` 一步撤销你的上一个请求及其修改的文件，无需信任模式。`/restore` 把整个工作区回滚到更早的快照；由于这会覆盖之后的编辑，它需要 `/trust on`
或“完全访问”权限。参见[回执](../RECEIPTS.md)。

可使用工具的 exec 运行无论采用哪种输出格式都会保存，因此在 `exec --auto` 之后，
包括使用 `--json` 时，`codewhale receipts --last` 都能正常工作。仅作答且使用文本输出或
`--json` 的运行，只有在恢复对话或开始压缩对话时才会保存；使用
`--output-format stream-json` 可始终保存这些运行，供回执或 `exec --continue` 使用。

### 更长的任务

`/goal` 设定一个跨回合持续有效的目标。Codewhale 可以把工作拆分给
[子智能体](../SUBAGENTS.md)，运行由多个智能体组成的 [Fleet](../FLEET.md)，
或者按照提交到仓库中的[工作流](../WORKFLOW_AUTHORING.md)执行。

### 从其他智能体迁移

Codewhale 会自动读取 `AGENTS.md`。为其他工具编写的指令文件，例如 `CLAUDE.md`、
`.cursorrules` 或 `.github/copilot-instructions.md`，只有在你于配置中设置
`project_instruction_imports = ["claude"]`（或 `"cursor"`、`"cline"`、`"windsurf"`、
`"copilot"`、`"all"`）主动启用后才会加载；参见 [config.example.toml](../../config.example.toml)。
如果 Codewhale 发现了未加载的此类文件，会给出警告。

`/import-claude` 会准备一份导入你的 Claude Code 设置和 MCP 服务器的方案，供你审阅。
`/plugin install` 接受包含技能、命令、智能体配置文件和 MCP 服务器的 Claude Code 插件，
但会拒绝需要钩子或 LSP 服务器的插件（[详情](../CLAUDE_PLUGIN_COMPAT.md)）。

### 扩展

连接 [MCP 服务器](../MCP.md)，添加[技能](../SKILLS.md)和[插件](../PLUGINS.md)，
并在会话和工具事件上运行[钩子](../HOOKS.md)。代码模式（Code mode，实验性，默认开启）
让模型编写一个 `execute_tools` 程序来调用多个 MCP 和插件工具；每次调用仍要遵守你的审批规则，
并保留各自的回执。独立安装的 Computer Use 插件（实验性，
仅限 macOS）可以观察和操作其他应用。在你审阅其访问权限并启用之前，它保持关闭
（[指南](https://github.com/codewhale-hq/codewhale-cu-plugin)）。

## 运行方式

| 命令 | 作用 |
| --- | --- |
| `codewhale` | 交互式终端应用 |
| `codewhale exec "…"` | 不启动界面，运行一个任务。单独使用时，它只作答而不使用工具；`--auto` 默认以“完全访问”权限启用工具，`--sandbox workspace-write` 对工具施加限制，`--output-format stream-json` 以 JSON 形式流式输出事件 |
| `codewhale web` | 监听 `127.0.0.1` 的本地 Web 界面（[指南](../WEB.md)） |
| `codewhale review --pr N` | 对 GitHub PR 进行仅供参考的审查。[GitHub 机器人设置](../GITHUB_ACTION.md#set-up-your-own-bot) 会生成一个固定版本的 workflow，用于自动审查和 `@codewhale review`，使用你自己的模型密钥和可选的 App 身份 |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md)，用于对话线程、事件、审批以及每个回合文件改动的本地 HTTP API |
| `codewhale serve --acp` | 通过 stdio 提供 Agent Client Protocol，供 Zed 等编辑器使用 |

终端应用、`exec`、`web` 和 Runtime API 共用同一个引擎（[架构](../ARCHITECTURE.md)）。
社区维护的
[VS Code 扩展](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
可以从侧边栏连接到它（[源码](https://github.com/HengQuWorld/CodeWhale-VSCode)）。
基于 Runtime API 构建的原生桌面应用正在开发中，目前还没有公开下载。

## 费用与账号

Codewhale 免费，采用 MIT 许可证。使用你自己的 API 密钥时，费用直接付给提供商；
本地模型不需要密钥。价格已知时，底部状态栏和 `/cost` 会显示一次会话的花费；
价格未知时会显示为未知，而不会显示为免费。

你不需要账号。通过 `/provider` 添加的密钥会留在你的机器上。

## 安全与隐私

在“询问”和“自动审核”权限下，只要系统提供操作系统沙箱，命令就会在其中运行：
macOS 上是 Seatbelt，Linux 上是 bubblewrap（需已安装）。Windows 目前还没有操作系统沙箱，
而“完全访问”会关闭沙箱。在沙箱内，shell 命令无法读取 `~/.ssh`、`~/.aws` 和 `.env`
文件等凭据存储；从 0.10.2 起，同样的规则也适用于终端会话和更多文件工具。
`/preview-request` 会显示下一次请求经过脱敏的预览，但不会发送它。

遥测默认开启。它发送汇总计数（版本和平台、会话时长和结果、你使用了哪些内置提供商、
功能和错误计数），并附带一个每 90 天轮换一次的随机安装 ID；它从不包含对话、代码、提示词、
模型名称、文件名或仓库名，也从不包含凭据。运行 `codewhale config set telemetry false` 即可关闭。启动时，Codewhale 还会
检查 GitHub 上是否有新版本；设置 `CODEWHALE_NO_UPDATE_CHECK=1` 可跳过这一检查。
更多信息见[授权顺序](../AUTHORIZATION_ORDER.md)、[沙箱](../SANDBOX.md)和
[遥测](../TELEMETRY.md)。

## 文档

| 从这里开始 | 参考 |
| --- | --- |
| [安装](../INSTALL.md) | [配置](../CONFIGURATION.md) |
| [提供商与本地模型](../PROVIDERS.md) | [架构](../ARCHITECTURE.md) |
| [模式与权限](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [快捷键](../KEYBINDINGS.md) | [插件开发](../PLUGIN_AUTHORING.md) |
| [GitHub PR 审查](../GITHUB_ACTION.md) | [全部文档](../README.md) |

## 社区

如果缺少某个提供商，或某个工作流用起来不顺手，
请[提交 issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)或
[提交 PR](../../CONTRIBUTING.md)。被合并的工作会保留贡献者的署名；
[仓库结构](../../CONTRIBUTING.md#project-structure)说明了各部分代码所在的位置。

欢迎在 [Discord](https://discord.gg/37gfS3ksug) 上交流，或在微信上添加维护者 Hunter
（`hunterbown`），申请加入 Whale Brothers 群。

## 历史与许可证

Codewhale 最初名为 `deepseek-tui`，至今仍会读取该项目的配置和会话。它是独立维护的项目，
与 DeepSeek 或任何其他模型提供商均无隶属关系。感谢[每一位贡献者](../CONTRIBUTORS.md)。

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE)。改编自其他开源项目的代码列在[第三方声明](../THIRD_PARTY_NOTICES.md)中。
