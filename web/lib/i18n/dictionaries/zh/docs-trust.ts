import type { DocsTrustDict } from "../types";

/** 「了解哪些数据会离开本机」页的简体中文词典；与 `en/docs-trust.ts` 逐段对应。 */
export const docsTrust: DocsTrustDict = {
  metaTitle: "了解哪些数据会离开本机 · Codewhale 文档",
  metaDescription:
    "哪些内容留在本地、模型提供商会收到什么、用量统计发送什么以及如何关闭、审计日志在哪里，以及如何报告安全漏洞。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "了解哪些数据会离开本机",
  lede:
    "Codewhale 在你的电脑上运行，只与你选择的模型提供商通信。这一页按目前的实际实现，列出数据分别去了哪里、匿名用量统计包含什么，以及如何关闭它。",
  sections: [
    {
      "id": "preview-request",
      "title": "发送前查看请求",
      "blocks": [
        {
          "p": "/preview-request 展示下一个主智能体回合的脱敏清单。这是供人使用的命令，不会发送请求，也不会写入对话或 Work 状态。"
        },
        {
          "code": "/preview-request\n/preview-request json\n/preview-request json --prompt explain this project",
          "lang": "Codewhale"
        },
        {
          "note": "未提供 --prompt 时，路由和请求体不可用。已配置的分类器、钩子或待处理运行状态也可能阻止精确预览；清单会说明原因。"
        },
        {
          "p": "[阅读请求预览约定](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/PREVIEW_REQUEST.md)"
        }
      ]
    },
    {
      id: "boundaries",
      title: "你的工作内容去了哪里",
      blocks: [
        {
          rows: [
            ["留在本机", "运行时、你的工作区、会话历史、快照和审计日志。"],
            ["发给你的提供商", "每个回合所需的上下文——你的消息、Codewhale 为这一回合读取的文件和工具结果——直接发给你选定的提供商，中间没有 Codewhale 中转。"],
            ["完全留在本地", "使用你自己运行的模型时，推理完全不离开你的机器。"],
            ["无需账户", "在本地安装和运行 Codewhale 不需要 Codewhale 账户。"],
            ["Plan 模式", "不能修改文件，也不能运行 shell 命令。但它被允许进行的资料查询仍可能访问外部服务。"],
          ],
        },
        {
          p: "你添加的工具可能把数据发到别处：MCP 服务器、网页搜索或钩子都以你的权限运行，能访问到它们能访问的一切。只添加你信任的工具。",
        },
      ],
    },
    {
      id: "telemetry",
      title: "了解用量统计会发送什么",
      blocks: [
        {
          p: "Codewhale 默认开启匿名用量统计，并在你第一次启动时明确告知，写明处理方是 Codewhale 和 PostHog。它具体包含：",
        },
        {
          rows: [
            ["从不发送", "提示词、回复、代码、diff、文件内容、文件名/仓库名/分支名、路径、模型 id、MCP 服务器名称、API 密钥或令牌、错误信息文本、按键记录，以及任何按回合或按工具的时间线。"],
            ["开启时发送", "版本与平台类别、会话时长与结果、以固定类别统计的功能与错误计数，以及一个每 90 天更换一次的随机安装 id。"],
            ["发往哪里", "`https://telemetry.codewhale.net/v1/telemetry`，一个第一方服务，源码就在仓库中。它不存储 IP 地址、国家或位置，不保留请求日志，数据保存三个月。"],
            ["PostHog", "只有在服务运营方另行配置后，才会转发给 PostHog；转发的字段完全相同，不会多出任何内容。"],
          ],
        },
      ],
    },
    {
      id: "turn-off",
      title: "关闭用量统计",
      blocks: [
        {
          code: `codewhale config set telemetry false   # stop, and erase the local id and buffer
CODEWHALE_TELEMETRY=0 codewhale        # stop for this process, erase nothing`,
          lang: "终端",
        },
        {
          p: "配置项是持久的选择：之后的版本都会沿用，命令行参数或环境变量也无法把它重新打开。你也可以在 `/settings` 中切换。关闭后，保存在你机器上的数据会被删除；已经发送的记录只与刚被删除的那个随机 id 关联，并会在三个月后过期。",
        },
        {
          p: "想确切看到会发送什么、但又不真的发送，可以在 `~/.codewhale/config.toml` 中设置 `telemetry_endpoint = \"\"`。此后每一批数据都会逐字节写入本机的 `$CODEWHALE_HOME/telemetry/dryrun.jsonl`，不会建立任何网络连接。",
        },
      ],
    },
    {
      id: "audit",
      title: "查看本地审计日志",
      blocks: [
        {
          p: "凭据、审批和提权相关的事件会追加写入 `$CODEWHALE_HOME/audit.log`（默认是 `~/.codewhale/audit.log`）。写入是尽力而为的：写入失败时会记录失败，而不是隐瞒。这份日志从不离开你的机器。",
        },
      ],
    },
    {
      id: "report",
      title: "报告安全漏洞",
      blocks: [
        {
          p: "请把安全问题发送到下方邮箱，不要公开提交 issue。请附上你的 Codewhale 版本（`codewhale --version`），如有复现步骤也请一并提供。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/sandbox",
      label: "限制命令的访问范围",
      note: "各平台上操作系统沙箱实际限制了什么。",
    },
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "决定 Codewhale 不经询问可以做什么。",
    },
    {
      href: "/docs/auth",
      label: "连接模型提供商",
      note: "选择由谁接收你的回合内容——或者用本地模型让它留在本机。",
    },
  ],
  sourceNote:
    "来源文档：docs/public-surface-facts.json（trust）、docs/TELEMETRY.md、docs/SANDBOX.md · 修改时同步更新 docs-map.ts。",
};
