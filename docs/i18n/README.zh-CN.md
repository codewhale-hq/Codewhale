<!-- source: README.md sha256:308ec0b2a2e8 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**开源 Rust 终端编程智能体。支持任意模型、多智能体编队、操作系统沙盒与可验证回执。**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[网站](https://codewhale.net) · [文档](../README.md) · [更新日志](../../CHANGELOG.md) · [参与贡献](../../CONTRIBUTING.md)

[English](../../README.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>本翻译为机器草稿，尚未经母语者审校。</sub>

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 在终端中运行本地 Ollama (qwen3:4b) 完成真实任务后的界面：它编辑了 ls.py，在运行 python3 -m unittest -v test_ls.py 前等待批准，2 项测试全部通过并汇报了结果。">

<sub>在已发布的 Codewhale v0.10.4 (48669ad108fa) 上使用本地 Ollama (qwen3:4b) 以 Accept Edits 模式运行的真实会话。</sub>

</div>

## 安装

上方安装脚本会在 macOS 和 Linux 上将经过校验和验证的 `codewhale` 及其短别名 `codew` 安装到 `~/.local/bin`。如果终端提示 `command not found`，请将 `~/.local/bin` 加入 `PATH`（[详情](../INSTALL.md#put-it-on-your-path)）。运行 `codewhale update` 可原地升级。

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

预编译二进制文件覆盖 Linux、macOS 和 Windows（x64 与 arm64）。Scoop、Nix、Linux Homebrew、Android/Termux（预览版）与手动下载方式见 [docs/INSTALL.md](../INSTALL.md)，容器镜像见 [docs/DOCKER.md](../DOCKER.md)。每台机器请仅使用一种安装方式，以免 `PATH` 中出现两个相互覆盖的二进制文件。

## 快速开始

1. 提交或暂存当前改动，然后在 git 仓库根目录运行 `codewhale`。
2. 运行 `/provider`（或按 `F3`）选择模型提供商、粘贴 API 密钥并选择模型。默认提供商为 DeepSeek；若未配置密钥且本地 Ollama 服务可用，Codewhale 会在你发送首条消息时提供其本地模型。
3. 输入任务：

```text
Fix the failing tests and explain what changed.
```

只读命令会立即执行（Ask 模式下每条命令都会先询问）。会修改状态的 Shell 命令会先请求批准：`y` 允许本次执行，`a` 允许本会话内同类命令，`n` 拒绝。运行 `/undo` 可撤销上一轮操作及其文件修改，运行 `/help` 可查看命令和快捷键。

## 核心能力

- **50+ 内置提供商与本地模型：** 支持 Anthropic、OpenAI、DeepSeek（默认 `deepseek-flash` / V4.1 Flash，[详情](../PROVIDERS.md#deepseek-v41-flash)）、Google、Mistral、Moonshot、OpenRouter、xAI、任意 OpenAI 兼容接口，以及 Ollama、vLLM 或 SGLang 等本地运行时。使用 `/model` 随时切换模型，`/model auto`（`/router`）按轮次自动路由，`/turn inspect` 查看耗时与费用。参见 [提供商文档](../PROVIDERS.md)。
- **回执与回滚：** `/receipts`（或退出后运行 `codewhale receipts --last`）记录修改的文件、执行的命令、Web/MCP 调用、子智能体、批准记录与失败项。`/undo` 一步撤销上一轮修改；`/restore` 将工作区回滚到更早快照（需启用 `/trust on` 或 Full Access）。参见 [回执文档](../RECEIPTS.md)。
- **子智能体、编队与工作流：** 通过 `/goal` 跨轮次跟踪目标，将任务委派给 [子智能体](../SUBAGENTS.md)，调度按角色与模型分工的 [Fleet 编队](../FLEET.md)，或执行仓库内版本化的 [工作流](../WORKFLOW_AUTHORING.md)。
- **MCP、技能、钩子与插件：** 连接 [MCP 服务器](../MCP.md)，加载 [技能](../SKILLS.md)、[插件](../PLUGINS.md) 与生命周期 [钩子](../HOOKS.md)。实验性代码模式（`execute_tools`）可在常规批准规则下批量调用多个 MCP/插件工具。可选的 macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) 默认关闭，需经你审查权限后手动启用。
- **无缝兼容：** 自动读取 `AGENTS.md`。如需加载 `CLAUDE.md`、`.cursorrules` 或 `.github/copilot-instructions.md`，可在 [config.example.toml](../../config.example.toml) 中配置 `project_instruction_imports`。使用 `/import-claude` 导入 Claude Code 设置与 MCP 服务器，或通过 `/plugin install` 安装 Claude Code 插件（技能、命令、智能体配置、MCP）与 DeepSeek Harness (DSH) 插件（[详情](../CLAUDE_PLUGIN_COMPAT.md)）。

## 运行入口

| 命令 | 用途 |
| --- | --- |
| `codewhale` | 交互式终端应用 |
| `codewhale exec "…"` | 适用于脚本和 CI 的无界面单任务运行器（[指南](../INSTALL.md#headless-scripts-ci)；未配沙盒时 `--auto` 默认使用 Full Access） |
| `codewhale web` | 运行在 `127.0.0.1` 的本地 Web 客户端（[指南](../WEB.md)） |
| `codewhale review --pr N` | 建议性 GitHub PR 审查；[GitHub 机器人配置](../GITHUB_ACTION.md#set-up-your-own-bot) 使用你自己的密钥运行 |
| `codewhale app-server --http` | 本地 HTTP [Runtime API](../RUNTIME_API.md)，提供线程、事件、批准与每轮 diff 接口 |
| `codewhale serve --acp` | 通过 stdio 提供 Agent Client Protocol，适配 Zed 等编辑器 |

所有入口共享同一个 Rust 引擎（[架构说明](../ARCHITECTURE.md)）。社区维护的 [VS Code 扩展](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)（[源码](https://github.com/HengQuWorld/CodeWhale-VSCode)）也连接到同一引擎。当前发布进展与桌面端/云端状态请见 [最新发布说明](https://github.com/codewhale-hq/Codewhale/releases/latest) 与 [路线图](../ROADMAP.md)。

## 权限、沙盒与隐私

按 `Shift+Tab` 切换权限模式：**Ask → Accept Edits → Plan → Auto-Review → Full Access**（`Tab` 仅用于补全）。**Accept Edits**（默认）会直接应用 git 仓库内的文件修改，执行命令前询问。**Ask** 在每次文件修改与命令执行前询问。**Plan** 为只读模式。**Auto-Review** 通过规则与审查模型自动放行常规调用并拦截高风险操作。**Full Access** 跳过询问并关闭操作系统沙盒（仓库规则与托管策略仍生效）。详见 [权限模式](../MODES.md)。

在 Ask、Accept Edits 和 Auto-Review 模式下，命令会在可用的操作系统沙盒中运行：macOS 使用 Seatbelt，Linux 使用 bubblewrap（Windows 暂不支持 OS 沙盒）。沙盒中的命令和终端会话无法读取 `~/.ssh`、`~/.aws` 或 `.env` 等凭据路径。运行 `/preview-request` 可在发送前查看脱敏后的请求预览。

通过 `/provider` 添加的 API 密钥仅保存在本机，本地模型无需密钥。`/cost` 和页脚会在已知价格时统计会话费用（未知价格会明确显示为未知，绝不显示为免费）。

聚合遥测默认开启（版本、操作系统、会话时长与结果、内置提供商 ID、内置公开目录中的模型 ID（自定义、私有或本地模型记为 `other`）、汇总 token 数量、内置工具调用与错误计数、功能与错误计数；每 90 天轮换的随机安装 ID；绝不上传代码、提示词、对话内容、自定义/私有模型名称、文件/仓库/分支名称、凭据或 IP 地址）。运行以下命令可关闭：

```bash
codewhale config set telemetry false
```

启动时 Codewhale 还会检查 GitHub 是否有新版本（设置 `CODEWHALE_NO_UPDATE_CHECK=1` 可跳过），并每 24 小时刷新一次公开的 `models.dev` 模型目录（设置 `CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` 或 `[catalog] auto_refresh = false` 可跳过；在 CI 环境和 `DO_NOT_TRACK` 下自动禁用）。详情参见 [授权顺序](../AUTHORIZATION_ORDER.md)、[沙盒机制](../SANDBOX.md) 与 [遥测说明](../TELEMETRY.md)。

## 文档

以下指南跟随 `main` 分支更新，可能包含尚未进入最新正式版的功能。

| 快速上手 | 参考文档 |
| --- | --- |
| [用户指南](../GUIDE.md) | [配置说明](../CONFIGURATION.md) |
| [安装指南](../INSTALL.md) | [架构设计](../ARCHITECTURE.md) |
| [提供商与本地模型](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [模式与权限](../MODES.md) | [插件开发](../PLUGIN_AUTHORING.md) |
| [快捷键](../KEYBINDINGS.md) | [回执说明](../RECEIPTS.md) |
| [GitHub PR 审查](../GITHUB_ACTION.md) | [全部文档](../README.md) |

## 贡献与许可证

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale 贡献者墙"></a>

[贡献者墙](../CONTRIBUTORS.md)（`scripts/contributor-wall.py`）记录所有代码提交、共同作者、延续合入的 PR 与署名反馈。Bug 反馈与功能建议请前往 [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose)，问题讨论请前往 [Discussions](https://github.com/codewhale-hq/Codewhale/discussions)，PR 规范与仓库结构见 [CONTRIBUTING.md](../../CONTRIBUTING.md)（[项目结构](../../CONTRIBUTING.md#project-structure)）。欢迎加入 [Discord](https://discord.gg/37gfS3ksug)，或添加微信 `hunterbown` 加入 Whale Brothers 交流群。

Codewhale 起源于 `deepseek-tui`，并保持对其配置和会话的向后兼容。本项目由社区独立维护，与 DeepSeek 或任何其他模型提供商无隶属关系。

采用 [MIT](../../LICENSE) 许可证。第三方声明见 [内嵌源码声明](../../THIRD_PARTY_NOTICES.md) 与 [改编设计声明](../THIRD_PARTY_NOTICES.md)。
