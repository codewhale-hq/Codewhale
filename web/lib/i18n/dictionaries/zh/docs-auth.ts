import type { DocsAuthDict } from "../types";

/** 「连接模型提供商」页的简体中文词典；与 `en/docs-auth.ts` 逐段对应。 */
export const docsAuth: DocsAuthDict = {
  metaTitle: "连接模型提供商 · Codewhale 文档",
  metaDescription:
    "为 Codewhale 接上模型：保存提供商密钥、查看当前使用的是哪把密钥，或者不用密钥直接运行本地模型。Codewhale 账户是可选的。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "连接模型提供商",
  lede:
    "Codewhale 需要一个模型来回答问题。你可以为云端提供商保存一把密钥，也可以让它连接你本机运行的模型。费用由你直接付给提供商，不经过 Codewhale 账户。",
  sections: [
    {
      id: "save-key",
      title: "保存提供商密钥",
      blocks: [
        {
          p: "先在提供商那里申请 API 密钥，然后保存。Codewhale 会提示你输入密钥，输入内容不会回显。DeepSeek 是默认提供商，下面以它为例。",
        },
        {
          code: `codewhale auth set --provider deepseek
codewhale auth status --provider deepseek`,
          lang: "终端",
        },
        {
          p: "`auth status` 会说明当前密钥来自哪里——配置文件、密钥存储还是环境变量——并且只显示最后四位。在脚本里可以用 `--api-key-stdin` 通过管道传入密钥，不必手动输入。",
        },
        {
          p: "也可以在 Codewhale 里连接：按 F3（或输入 `/provider`），选择提供商，粘贴密钥，再选一个模型。",
        },
      ],
    },
    {
      id: "which-key",
      title: "弄清用的是哪把密钥",
      blocks: [
        { p: "同一个提供商的密钥如果设在多处，按下面的顺序取第一个找到的：" },
        {
          steps: [
            "命令行上的 `--api-key`，只对这一次运行生效。",
            "`~/.codewhale/config.toml` 中的 `api_key`。",
            "`codewhale auth set` 写入的密钥存储。",
            "提供商自己的环境变量，例如 `DEEPSEEK_API_KEY`。",
          ],
        },
        {
          p: "因此，导出一个新的环境变量并不会替换你之前保存的密钥。如果换过的密钥一直报错，先用 `auth status` 看看生效的是哪个来源，再保存新密钥，或清除已存的旧密钥：",
        },
        { code: "codewhale auth clear --provider deepseek", lang: "终端" },
        { note: "显示凭据来源不代表提供商已经接受该密钥。`doctor --json` 只做离线结构检查；`doctor --probe-api` 会连接所选服务，可能产生提供商用量。" },
        {
          p: "在 Linux 上，密钥存储是 `~/.codewhale/secrets/` 下的一个私有文件（权限 0600），而不是系统钥匙串。`codewhale doctor --probe-api` 会发起一次测试调用，确认密钥和网络都正常。",
        },
      ],
    },
    {
      id: "other-providers",
      title: "使用其他提供商或本地模型",
      blocks: [
        {
          p: "`codewhale auth list` 会列出各提供商报告的凭据状态。API 密钥路由使用 `codewhale auth set --provider <name>` 或文档指定的环境变量；OAuth 与本地路由有各自的设置方式。",
        },
        {
          p: "为其他托管提供商保存密钥，再选择它和模型：",
        },
        {
          code: `codewhale auth set --provider anthropic
codewhale --provider anthropic --model claude-sonnet-5-5`,
          lang: "终端",
        },
        {
          p: "你自己运行的服务（vLLM、SGLang、Ollama）默认无需 API 密钥。连接回环地址，再选择该服务提供的准确模型标签：",
        },
        {
          code: `codewhale --provider vllm --model <model-tag>`,
          lang: "终端",
        },
        {
          p: "[模型页面](/models) 列出了各家提供商、本地部署方式，以及如何在会话中切换模型。",
        },
      ],
    },
    {
      id: "account",
      title: "Codewhale 账户",
      blocks: [
        {
          p: "提供商密钥和 Codewhale 账户是两回事。本地使用从不需要账户；任何人都可以登录，云端计算机仅限受邀使用。提供商密钥通过 `codewhale auth set` 保存在你的机器上。",
        },
        {
          rows: [
            ["codewhale login", "在浏览器中用一次性验证码登录。"],
            ["codewhale account status", "查看当前配置档登录的是哪个账户。"],
            ["codewhale account logout", "移除当前配置档的登录会话。"],
          ],
          codeTerms: true,
        },
        {
          p: "`codewhale login` 不接收提供商密钥，提供商密钥一律通过 `codewhale auth set` 保存。登录会话优先保存在操作系统的凭据管理器中；没有凭据管理器时（例如通过 SSH 或在容器里），则保存在 Codewhale 的私有密钥文件中。每个入口的登录步骤见[登录 Codewhale](/docs/account)。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/guide",
      label: "开始第一个任务",
      note: "在项目里打开 Codewhale，交给它一件具体的事。",
    },
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "决定 Codewhale 运行命令之前是否先问你。",
    },
    {
      href: "/docs/trust",
      label: "了解哪些数据会离开本机",
      note: "提供商会收到什么、哪些留在本地，以及如何关闭用量统计。",
    },
  ],
  sourceNote:
    "来源文档：docs/INSTALL.md §8、docs/PROVIDERS.md、docs/CODEWHALE_AGENT.md · 修改时同步更新 docs-map.ts。",
};
