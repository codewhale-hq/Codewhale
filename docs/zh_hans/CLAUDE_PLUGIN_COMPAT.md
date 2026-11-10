# Claude 插件兼容性

> 英文原文：[CLAUDE_PLUGIN_COMPAT.md](../CLAUDE_PLUGIN_COMPAT.md)。
> 最后与英文同步日期（last synced with English revision）：2026-11-10。

Claude Code 的技能（skill）目录如果只是普通的 `SKILL.md` 目录，Codewhale 会把它当作
指令包来处理。Codewhale 不运行 Claude Code 的插件运行时。

## 支持的内容

- 由常规技能注册表发现的工作区级或全局 `.claude/skills/<name>/SKILL.md` 目录。
- 从 GitHub 或 tarball 安装的、包含一个选定技能目录的包，例如
  `skills/<name>/SKILL.md`、`.agents/skills/<name>/SKILL.md`、
  `.claude/skills/<name>/SKILL.md`，或以 `skills/<name>/SKILL.md` 结尾的
  嵌套包布局。
- 选定技能目录中的配套文件，例如 `references/`、`examples/`，
  或只有在该技能被显式加载并受信任之后才会用到的脚本。

## 兼容的插件包

自 v0.9.13 起，`/plugin install` 接受[安装插件](./PLUGINS.md)中描述的声明式子集：
嵌套的 `.claude-plugin/plugin.json` 元数据，根目录下的技能、命令和
智能体配置（agent profile），以及内联 MCP 声明或 `.mcp.json`。
Claude 市场目录中的本地来源，会从仓库根目录（而不是 `.claude-plugin` 目录）解析。

MCP 请求头的值如果是完整的 `${ENV_NAME}` 引用（包括 `Authorization: Bearer ${ENV_NAME}`），
会映射为 Codewhale 基于环境变量的凭据。字面量凭据值和复合请求头模板会被拒绝；
导入过程从不读取所引用的环境变量。

导入器沿用 Codewhale 现有的适配器、审查哈希和启用机制。
Claude 市场的标签或默认值不授予任何权限。Codewhale 不会从其他应用的插件根目录
扫描插件包，也不会自动激活插件包。

## 不支持的运行时特性

导入器会拒绝钩子（hooks）、LSP 声明、自定义 MCP 文件路径和
`${CLAUDE_PLUGIN_ROOT}` 展开。它不会执行插件构建步骤，不会运行 TypeScript 智能体、
仪表盘服务器或受令牌保护的服务进程，也不维护共享插件状态。Claude 特有的 frontmatter 行为
（例如 `model: inherit`）不构成额外的运行时契约。远程 MCP 的身份验证仍遵循
Codewhale 现有的插件凭据边界；安装过程不会完成 OAuth，也不会借用其他应用的凭据。

`/skill install` 仍是独立的、一次只装一个技能的操作：遇到包含多个技能的插件归档，
它会直接拒绝，而不是悄悄选中其中一个目录、丢掉其他组件。受支持的插件包请用
`/plugin install`；如果仓库依赖不受支持的运行时行为，就单独迁移一个明确指定的技能目录。

发现、审查与激活见 [PLUGIN_BUNDLES.md](./PLUGIN_BUNDLES.md)。
