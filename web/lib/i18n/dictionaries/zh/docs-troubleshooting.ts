import type { DocsTroubleshootingDict } from "../types";

/** 「排查问题」页的简体中文词典；与 `en/docs-troubleshooting.ts` 逐段对应，报错原文保持英文。 */
export const docsTroubleshooting: DocsTroubleshootingDict = {
  metaTitle: "排查问题 · Codewhale 文档",
  metaDescription:
    "用一条命令诊断 Codewhale，再逐一解决常见问题：找不到命令、没有回复、密钥被拒、网络错误、回合卡住、会话无法恢复，以及 MCP 服务器问题。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "排查问题",
  lede: "先运行一条诊断命令，再在下面找到你遇到的症状。每条解决办法都写明了你会看到的确切报错。",
  sections: [
    {
      id: "diagnose",
      title: "运行诊断",
      blocks: [
        {
          code: `codewhale --version
codewhale doctor
codewhale doctor --probe-api                  # one real test call to your provider
codewhale auth status --provider deepseek     # which key is in use`,
          lang: "终端",
        },
        {
          p: "`codewhale doctor --json` 会生成一份不含任何密钥的诊断包，可以直接附到 issue 上。单独运行 `doctor` 不会告诉你当前用的是哪把密钥，即使没有密钥也会正常退出；查密钥请用 `auth status`。",
        },
      ],
    },
    {
      id: "install",
      title: "安装与更新",
      blocks: [
        {
          rows: [
            ["`codewhale: command not found`", "当前终端的 PATH 中没有 `~/.local/bin`。把 `export PATH=\"$HOME/.local/bin:$PATH\"` 加入 shell 配置文件，然后打开一个新终端。"],
            ["`npm error code EACCES`", "你的 Node 由系统管理。不要用 sudo：用 `npm config set prefix \"$HOME/.npm-global\"` 让 npm 安装到你自己的目录，把其中的 `bin` 加入 PATH，再重新安装。"],
            ["`refusing to replace existing ~/.local/bin/codewhale`", "那里已经装了另一个版本。运行 `codewhale update`，或者先删除旧的二进制文件。"],
            ["`checksum mismatch`", "下载的文件损坏或被篡改，因此没有安装任何东西。请重试；如果一再出现，不要使用镜像源。"],
            ["`The package-managed executable was not changed.`", "你是通过 npm、Cargo 或 Homebrew 安装的，请用对应的工具更新，例如 `npm install -g codewhale`。"],
          ],
        },
      ],
    },
    {
      id: "model",
      title: "没有回复，或密钥被拒",
      blocks: [
        {
          rows: [
            ["一直没有回复", "先运行 `codewhale --version` 查看版本：在 v0.10.2 中，未连接模型时消息会保留在输入框，并打开提供商设置，请连接模型后重新发送。如果已完成设置，先用 `codewhale auth status --provider <name>` 查看所选提供商的凭据状态，再用 `codewhale doctor --probe-api` 检查当前托管服务，或用 `codewhale doctor --probe-local` 检查本地模型；这些检查会连接相应服务。"],
            ["`API key not found`", "哪里都找不到密钥。用 `codewhale auth set --provider <name>` 保存一把。"],
            ["`Authentication Fails … is invalid`", "密钥错误或已被吊销。运行 `auth status` 查看用的是哪个来源——已保存的密钥优先于环境变量——然后保存正确的密钥，或运行 `codewhale auth clear --provider <name>`。"],
            ["`Network error: SSE stream request failed …`", "通常是连不上提供商。用 `curl -sI https://api.deepseek.com` 检查（返回 401 说明能连通）。在代理后面时请导出 `HTTPS_PROXY`；在 Windows 或限制严格的代理环境下，可以试试 `CODEWHALE_FORCE_HTTP1=1`。"],
          ],
        },
      ],
    },
    {
      id: "turn",
      title: "回合卡住了",
      blocks: [
        {
          list: [
            "按 Esc 取消当前回合。Esc 会先关闭已打开的菜单，所以如果开着菜单，需要再按一次。",
            "如果是一条耗时很长的 shell 命令拖住了回合，按 Ctrl-B 把它移到后台。回合会继续进行，`/jobs` 中可以看到这条命令。",
            "`/retry` 会重新发送上一次请求。",
          ],
        },
        {
          p: "需要详细记录时，用 `RUST_LOG=codewhale_tui=debug` 启动 Codewhale（排查连接重试用 `RUST_LOG=codewhale_tui::client=debug`）。日志写在 `~/.codewhale/logs/` 下。",
        },
      ],
    },
    {
      id: "sessions",
      title: "恢复会话",
      blocks: [
        {
          code: `codewhale sessions            # list saved sessions
codewhale resume <id>         # an id or a unique prefix
codewhale -c                  # the latest session in this folder`,
          lang: "终端",
        },
        {
          p: "在 Codewhale 中按 Ctrl-R 可以打开会话选择器。如果 `codewhale exec --continue` 报出 `No saved sessions found for workspace`，说明之前那次是普通的 `exec` 运行，它不会被保存；想接着运行的任务请使用 `--output-format stream-json`。",
        },
        {
          p: "离线时发送的消息会随会话一起保存在队列中，`/queue list` 可以查看。网络恢复后，用 `/queue edit <n>` 打开其中一条，按 Enter 发送。",
        },
      ],
    },
    {
      id: "mcp",
      title: "MCP 工具不见了",
      blocks: [
        {
          list: [
            "修改 `mcp.json` 或服务器凭据后，运行 `/mcp reload`。`/mcp validate` 只会刷新界面上显示的内容。",
            "在 shell 中亲自运行一遍服务器命令，确认它能启动。",
            "如果缺少 `mcp.json`，可用 `codewhale mcp init` 创建。修复现有文件前请先备份；`--force` 会替换其中的配置。",
          ],
        },
      ],
    },
    {
      id: "docker",
      title: "在 Docker 中运行",
      blocks: [
        {
          code: `docker volume create codewhale-home
docker run --rm -it \\
  -e DEEPSEEK_API_KEY="$DEEPSEEK_API_KEY" \\
  -v codewhale-home:/home/codewhale/.codewhale \\
  -v "$PWD:/workspace" -w /workspace \\
  ghcr.io/hmbown/codewhale:v0.10.0`,
          lang: "终端",
        },
        {
          p: "此处使用 [Docker 指南](https://github.com/codewhale-hq/CodeWhale/blob/main/docs/DOCKER.md) 中记录的镜像与 v0.10.0 标签。镜像以非 root 用户运行；请将配置保存在命名卷中，每个项目使用独立卷，不要把密钥写入镜像。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/auth",
      label: "连接模型提供商",
      note: "保存密钥、查看当前使用的是哪一把，或改用本地模型。",
    },
    {
      href: "/install",
      label: "安装 Codewhale",
      note: "所有安装方式，以及每一步应有的输出。",
    },
    {
      href: "/docs/review",
      label: "查看改动",
      note: "把文件回滚到出问题之前那个回合的快照。",
    },
  ],
  sourceNote: "来源文档：docs/INSTALL.md §13、docs/OPERATIONS_RUNBOOK.md、docs/DOCKER.md · 修改时同步更新 docs-map.ts。",
};
