import type { DocsConfigurationDict } from "../types";

/** 「修改设置」页的简体中文词典；与 `en/docs-configuration.ts` 逐段对应。 */
export const docsConfiguration: DocsConfigurationDict = {
  metaTitle: "修改设置 · Codewhale 文档",
  metaDescription:
    "找到 Codewhale 的配置文件，在会话或 shell 中修改设置，只对单次运行试用某个设置，并了解仓库可以覆盖哪些设置。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "修改设置",
  lede:
    "大多数设置无需打开文件就能修改：在会话中用 `/config`，在 shell 中用 `codewhale config`。这一页介绍设置保存在哪里、如何修改，以及仓库能替你改什么、不能改什么。",
  sections: [
    {
      id: "file",
      title: "找到配置文件",
      blocks: [
        { p: "你的设置保存在 `~/.codewhale/config.toml`。可以打印确切路径，或直接用编辑器打开：" },
        { code: "codewhale config path\ncodewhale config edit", lang: "终端" },
        {
          p: "想使用别的文件，传入 `--config <path>` 或设置 `CODEWHALE_CONFIG_PATH`；两者同时存在时以命令行参数为准。Codewhale 曾名为 DeepSeek-TUI：如果只有旧的 `~/.deepseek/` 文件夹，仍会读取它，而新设置一律写入 `~/.codewhale/`。",
        },
      ],
    },
    {
      id: "change",
      title: "修改设置",
      blocks: [
        {
          p: "在会话中，`/config` 打开设置编辑器，`/settings` 打开设置界面。`/config audit` 会列出哪些设置可以在本次会话中修改、哪些可以保存、哪些要重启后才生效——手动编辑之前先看一下。",
        },
        { p: "在 shell 中：" },
        {
          code: `codewhale config get tools
codewhale config set telemetry false
codewhale config unset telemetry
codewhale config dump       # the settings in effect, secrets redacted
codewhale config doctor     # unknown keys, empty secrets, malformed URLs`,
          lang: "终端",
        },
        {
          p: "`config set` 只处理它认识的配置项；遇到其他配置项时，它会告诉你应该去编辑哪个 TOML 表，而不会自作主张。",
        },
      ],
    },
    {
      id: "one-run",
      title: "只对一次运行试用设置",
      blocks: [
        {
          p: "`--set KEY=VALUE` 只在这一次启动中修改设置，不保存任何内容。它支持 `provider`、`model`、`verbosity`、`approval_policy`、`sandbox_mode` 和 `telemetry`，可以重复使用。",
        },
        { code: "codewhale --set model=deepseek-flash --set sandbox_mode=read-only", lang: "终端" },
      ],
    },
    {
      id: "project",
      title: "与仓库共享设置",
      blocks: [
        {
          p: "仓库可以附带 `.codewhale/config.toml`，为所有在其中工作的人建议一些设置。只有少数配置项会生效，而且安全相关的设置只能更严格：",
        },
        {
          rows: [
            ["model", "这个仓库的默认模型。"],
            ["reasoning_effort", "例如为复杂的代码库设置 `\"high\"`。"],
            ["approval_policy, sandbox_mode", "只接受比你自己的设置更严格的值。"],
            ["allow_shell", "`false` 会关闭 shell 命令；`true` 会被忽略。"],
            ["max_subagents", "减少并行子智能体的数量，范围 1 到 128。"],
            ["notes_path", "把笔记保存在仓库里。"],
          ],
          codeTerms: true,
        },
        {
          p: "密钥、服务端点、提供商选择、MCP 服务器、钩子、技能和额外的说明文件，始终来自你自己的配置，因此克隆下来的仓库无法让 Codewhale 指向它自己的服务器或文件。启动时加上 `--no-project-config`，可以在这一次运行中忽略仓库的配置文件。",
        },
        {
          p: "项目说明——智能体在这个仓库里应该如何工作——请写在 `AGENTS.md` 中。运行 `/init` 即可生成一份。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/auth",
      label: "连接模型提供商",
      note: "保存密钥，并查看 Codewhale 当前使用的是哪一把。",
    },
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "大多数人最先修改的设置。",
    },
    {
      href: "/docs/hooks",
      label: "在事件发生时运行命令",
      note: "把你自己的脚本加入 `[hooks]` 表。",
    },
  ],
  sourceNote: "来源文档：docs/CONFIGURATION.md、docs/LEGACY_PATHS.md · 修改时同步更新 docs-map.ts。",
};
