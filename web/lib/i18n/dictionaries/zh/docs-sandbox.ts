import type { DocsSandboxDict } from "../types";

/** 「限制命令的访问范围」页的简体中文词典；与 `en/docs-sandbox.ts` 逐段对应。 */
export const docsSandbox: DocsSandboxDict = {
  metaTitle: "限制命令的访问范围 · Codewhale 文档",
  metaDescription:
    "了解 macOS、Linux 和 Windows 上用哪种操作系统沙箱包裹 shell 命令，在可选的平台上开启它，并决定命令能写到哪里。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "限制命令的访问范围",
  lede:
    "批准一条命令，决定的是它能不能运行；沙箱决定的是它运行之后能碰到什么。只要操作系统提供沙箱，Codewhale 就会使用它；没有沙箱时，也会如实告诉你。",
  sections: [
    {
      "id": "execution-policy",
      "title": "检查执行策略规则",
      "blocks": [
        {
          "code": "/permissions",
          "lang": "Codewhale"
        },
        {
          "p": "类型化权限依次按 User、Agent、BuiltinDefault 选择规则。同一来源层中，deny 优先于 ask 与 allow；匹配精度用于打破同级平局。"
        },
        {
          "p": "硬性拒绝的命令前缀不能被类型化 allow 覆盖。allow 不会移除后续安全检查、仓库限制或沙箱边界。"
        },
        {
          "p": "[阅读授权顺序与匹配规则](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/AUTHORIZATION_ORDER.md)"
        }
      ]
    },
    {
      id: "platforms",
      title: "查看你的平台提供了什么",
      blocks: [
        {
          rows: [
            ["macOS", "Seatbelt，启动检查通过后自动启用。命令可以广泛读取，写入范围由沙箱模式限定，只有模式允许时才能联网。"],
            ["Linux", "bubblewrap，安装并验证可用后自动启用（退出见下文）。不可用时，命令在没有操作系统沙箱的情况下运行，Codewhale 会把工作区限制显示为未强制执行。"],
            ["Windows", "没有操作系统沙箱。Codewhale 会把工作区限制显示为未强制执行，你的审批设置和 Windows 自身的权限仍然有效。"],
            ["外部服务", "设置 `sandbox_backend = \"opensandbox\"` 后，shell 命令会在你配置的 OpenSandbox 兼容服务上运行；隔离效果由该服务负责保证。"],
          ],
        },
        { p: "问问 Codewhale 它找到了哪一种：" },
        { code: "codewhale doctor\ncodewhale setup --status", lang: "终端" },
        {
          p: "两条命令报告的都是应用你的设置之后实际可用的沙箱。仓库里存在但没有接入执行路径的代码，Codewhale 从不把它算作沙箱。",
        },
      ],
    },
    {
      id: "linux",
      title: "启用（或关闭）Linux 沙箱",
      blocks: [
        { p: "安装 bubblewrap 后 Codewhale 默认使用它。想让 Linux 命令不被包装，在 `~/.codewhale/config.toml` 中加一行退出：" },
        {
          code: "sudo apt install bubblewrap",
          lang: "Debian / Ubuntu — 终端",
        },
        {
          code: "prefer_bwrap = false",
          lang: "~/.codewhale/config.toml — 可选：关闭 bubblewrap",
        },
        {
          p: "只有当 `/usr/bin/bwrap` 存在、可执行，且探测证明它能创建沙箱命名空间时，Codewhale 才会使用它——装了但被系统拦住的 bwrap 按不存在处理。此后，命令看到的是只读的系统视图，只能写入沙箱模式允许的位置，除非模式允许，否则无法联网。",
        },
      ],
    },
    {
      id: "mode",
      title: "决定命令能写到哪里",
      blocks: [
        { code: 'sandbox_mode = "workspace-write"', lang: "config.toml" },
        {
          rows: [
            ["read-only", "命令只能读，不能写。"],
            ["workspace-write", "命令只能写入工作区和临时文件夹，其他地方都不行。"],
            ["danger-full-access", "不使用操作系统沙箱。只在你不怕损坏的机器或容器里使用。"],
            ["external-sandbox", "你已经运行在隔离环境里，Codewhale 不再额外加一层。"],
          ],
          codeTerms: true,
        },
        {
          p: "前两种模式只有在沙箱可用时才会真正生效——在没有 bubblewrap 的 Linux 上，以及在 Windows 上，它们只是设置，背后没有操作系统沙箱。仓库自带的配置只能让模式更严格，不能更宽松。对单次无界面运行，可以在 `codewhale exec` 的提示词前传入 `--sandbox <mode>`。`--auto` 默认启用 Full Access（完全访问）：工具无需批准即可运行，shell 命令没有 Codewhale 沙箱，信任模式允许文件工具写入工作区以外的路径。除 `danger-full-access` 外的任何 `--sandbox`（或配置中对应的 `sandbox_mode`）都会保持信任模式关闭。使用 `--sandbox workspace-write` 可在沙箱可用时启用 shell 沙箱，并保留文件工具的工作区限制；`external-sandbox` 则依赖你自行提供的隔离。",
        },
      ],
    },
    {
      id: "limits",
      title: "了解局限",
      blocks: [
        {
          list: [
            "命令启动前会检查沙箱是否可用，但受主机策略或容器限制影响，沙箱仍可能在启动时失败。",
            "命令报出“Permission denied”并不能证明是沙箱拦下了它。只有沙箱自己报告的拒绝，Codewhale 才会标记为沙箱拒绝。",
            "任何沙箱都无法防御内核漏洞，也无法防住所有类型的资源耗尽。",
          ],
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "决定哪些命令需要停下来等你批准。",
    },
    {
      href: "/docs/trust",
      label: "了解哪些数据会离开本机",
      note: "提供商会收到什么、哪些留在本地，以及遥测会发送什么。",
    },
    {
      href: "/docs/configuration",
      label: "修改设置",
      note: "这些配置项写在哪里，以及仓库可以覆盖哪些。",
    },
  ],
  sourceNote: "来源文档：docs/SANDBOX.md、docs/CONFIGURATION.md · 修改时同步更新 docs-map.ts。",
};
