# Codewhale 简体中文文档阅读指南

> 这里是简体中文用户阅读 Codewhale 文档的入口。英文原版文档在文档根目录 [`docs/`](../)下，英文入口见 [docs/README.md](../README.md)。
> 本文档按经验水平组织阅读路径，并跟踪各文档的翻译状态。
> 下文的阅读顺序建议仅代表文档维护者的个人看法，与 Codewhale 官方立场无关。
> 若某篇文档尚无中文译文，请直接阅读英文原版；中文译文若与英文原版不一致，以英文原版为准。

---

## 一、零基础（完全没接触过 harness，甚至对 AI 编程毫无概念）

从零开始，先弄懂"它是什么、装在哪儿、怎么跑起来"。

1. [WINDOWS_BEGINNER.md](WINDOWS_BEGINNER.md) —— Windows 用户上手指南，完全零基础者可阅读此文档以入门
2. [HarmonyOS.md](./HarmonyOS.md) —— 鸿蒙设备安装说明，鸿蒙系统用户请重点关注
3. [INSTALL.md](./INSTALL.md) —— 所有受支持平台的安装方式与常见安装失败排查
4. [TERMUX.md](./TERMUX.md) —— 在 Android 手机（Termux）上安装运行，目前为预览支持
5. [CNB_MIRROR.md](./CNB_MIRROR.md) —— 国内网络下载慢或连不上 GitHub 时的镜像说明

## 二、入门用户（已经装好程序、但仍然对 harness 不够了解）

阅读以下文档，可以帮助你快速入门，掌握 Codewhale 的使用方法

1. [GUIDE.md](./GUIDE.md) —— 最基础的入门教程：首次运行、会话、命令与日常工作流，适用于所有平台
2. [KEYBINDINGS.md](./KEYBINDINGS.md) —— TUI 页面的快捷键列表，在这里可以了解 Codewhale 的日常操作方式
3. [MODES.md](./MODES.md) —— 各个模式（Plan / Work / Operate）与权限姿态的说明（强烈建议每个 Codewhale 用户都阅读此项）
4. [PROVIDERS.md](./PROVIDERS.md) —— 查看你所用模型的提供商是否在 Codewhale 官方支持的列表中，以及各提供商的配置方法
5. [DOCKER.md](./DOCKER.md) —— 用容器运行 Codewhale，含挂载、权限与常见坑
6. [ACCESSIBILITY.md](./ACCESSIBILITY.md) —— 无障碍与低动态模式、对比度与屏幕阅读器
7. [PET_MODE.md](./PET_MODE.md) —— Pet 模式：每个终端一头鲸鱼，一眼看清工作中、等你、完成或失败，按需查看回复与智能体

## 三、进阶用户（已经有充足了解，追求效率、安全与定制）

把 Codewhale 配置成最顺手的样子。

1. [CONFIGURATION.md](./CONFIGURATION.md) —— 完整配置参考（最大的文档，可分章节阅读）
2. [Fleet](./FLEET.md) —— Fleet：已保存的模型与角色名册，用于多智能体运行的成员选择
3. [FLEET_WORKFLOW_TUTORIAL.md](./FLEET_WORKFLOW_TUTORIAL.md) —— 从零跑通一次 Fleet 加 Workflow 编排
4. [MCP.md](./MCP.md) —— MCP（模型上下文协议）服务器接入
5. [SKILLS.md](./SKILLS.md) —— 技能（skill）的安装、管理与使用
6. [PLUGINS.md](./PLUGINS.md) —— 安装插件：来源、审查与启用策略
7. [PLUGIN_BUNDLES.md](./PLUGIN_BUNDLES.md) —— 插件包（bundle）格式、校验与信任边界
8. [SUBAGENTS.md](./SUBAGENTS.md) —— Fleet 与子智能体机制
9. [WORKFLOW_AUTHORING.md](./WORKFLOW_AUTHORING.md) —— 自己动手写一个 Workflow
10. [HOOKS.md](./HOOKS.md) —— 钩子机制与自动化
11. [TOOL_SURFACE.md](./TOOL_SURFACE.md) —— 工具面：AI 当前可用的工具契约
12. [AGENT_RUNTIME.md](./AGENT_RUNTIME.md) —— 智能体运行时：子智能体、`exec` 与 Fleet 的关系
13. [MEMORY.md](./MEMORY.md) —— 记忆系统：存取位置、检索方式与开关
14. [CACHE.md](./CACHE.md) —— 提示缓存：为什么前缀稳定能省钱
15. [SANDBOX.md](./SANDBOX.md) —— 沙箱与权限边界：各平台能隔离到什么程度
16. [CATALOG_REFRESH.md](./CATALOG_REFRESH.md) —— 模型目录从哪来、怎么刷新
17. [WEB.md](./WEB.md) —— 本地浏览器客户端：启动方式、会话验证边界与故障排查
18. [VOICE.md](./VOICE.md) —— 语气与终端视觉章程（产品文案与界面的一致性原则）

## 四、开发者（阅读源码或为 Codewhale 贡献）

为 Codewhale 贡献代码或做集成开发。

1. [ARCHITECTURE.md](./ARCHITECTURE.md) —— 架构总览
2. [CONTRIBUTING.md](../../CONTRIBUTING.md) —— 贡献指南：如何提交 Issue 与 PR、代码约定与验证门禁
3. [CODE_OF_CONDUCT.md](../../.github/CODE_OF_CONDUCT.md) —— 社区行为准则
4. [RUNTIME_API.md](./RUNTIME_API.md) —— Runtime API 与集成契约（供集成与二次开发）
5. [PLUGIN_AUTHORING.md](./PLUGIN_AUTHORING.md) —— 从最小的 Skills 示例开始编写、审查并启用插件
6. [BUILD_PERFORMANCE.md](./BUILD_PERFORMANCE.md) —— 构建性能：本仓库的编译缓存与目录拓扑
7. [LOCALIZATION.md](./LOCALIZATION.md) —— 本地化：网站、界面与文档三层表面的现状
8. [REBRAND.md](./REBRAND.md) —— 从 DeepSeek TUI 到 Codewhale 的更名历史与兼容路径
9. [AGENT_ETHOS.md](./AGENT_ETHOS.md) —— 智能体准则：Codewhale 希望智能体成为什么样的工作伙伴
10. [AUTHORIZATION_ORDER.md](./AUTHORIZATION_ORDER.md) —— 授权顺序：工具可用性、钩子、权限规则与审批姿态的判定次序
11. [AUTOMATIC_WORKFLOWS.md](./AUTOMATIC_WORKFLOWS.md) —— 自动工作流：多智能体编排，不必手写 `.workflow.js`
12. [COMMAND_CONTROL_PLANE.md](./COMMAND_CONTROL_PLANE.md) —— 共享的命令与控制平面契约
13. [ENVIRONMENTS.md](./ENVIRONMENTS.md) —— 特定环境的注意事项（各平台构建/测试差异）
14. [LEGACY_PATHS.md](./LEGACY_PATHS.md) —— 旧版 `.deepseek/` 兼容路径：审计与迁移状态
15. [LIVE_SMOKE.md](./LIVE_SMOKE.md) —— 可选的实时冒烟运行（手动、绝不自动化）
16. [OPERATIONS_RUNBOOK.md](./OPERATIONS_RUNBOOK.md) —— 运维手册：调试与事故响应
17. [WORKROOM_ARCHITECTURE.md](./WORKROOM_ARCHITECTURE.md) —— 工作间（Workroom）架构
18. [WORKROOM_SECURITY.md](./WORKROOM_SECURITY.md) —— Workroom 安全模型
19. [TELEMETRY.md](./TELEMETRY.md) —— 产品遥测：默认行为与关闭方式
20. [LSP_PHP_CUSTOM.md](./LSP_PHP_CUSTOM.md) —— LSP：PHP 支持与自定义语言服务器
21. [TOOL_LIFECYCLE.md](./TOOL_LIFECYCLE.md) —— 工具面生命周期策略（v0.8.53，历史设计记录，非当前运行时文档）

> 我们强烈建议，成为 Codewhale 贡献者之前，您需要具备一定的英语阅读能力。如果您在英语方面较为薄弱，当然可以使用 LLM 来翻译。但是在 LLM 翻译完原文之后，建议您强忍着看不懂外文的不适，即使皱着眉头，也要审查一遍 LLM 翻译后的语义是否与你的原文语义相同。LLM 幻觉是会把事情搞砸的。

---

## 翻译状态

若想问询文档的翻译排期与逐篇状态，可在 [issue #5482](https://github.com/codewhale-hq/CodeWhale/issues/5482) 跟踪。

截至 2026-09-29，本目录已收录上文列出的全部中文译文。译文可能落后于英文原版：请以各篇顶部的 `last synced with English revision` 日期为准，并对照英文原版核对命令、参数与配置项。

## 约定

- 每个中文译文都放在 `docs/zh_hans/` 下，保留与英文源相同的文件名主干，便于一一对应。
- 英文文档顶部有一条语言切换横幅（例如安装文档中的“阅读简体中文版”链接）。
- 中文文档会链接回英文原版，并标注 "last synced with English revision" 日期，让过期一目了然。
- 旧位置的 `.zh-CN.md` 文件保留为重定向占位页，保留一个发布周期后移除。

本文档更新于 2026 年 9 月 29 日
Last Updated on September 29, 2026
