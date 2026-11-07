import type { DocsMcpDict } from "../types";

/** 「用 MCP 连接工具」页的简体中文词典；与 `en/docs-mcp.ts` 逐段对应。 */
export const docsMcp: DocsMcpDict = {
  metaTitle: "用 MCP 连接工具 · Codewhale 文档",
  metaDescription:
    "连接本地和远程 MCP 服务器，了解认证与插件兼容范围，并排查工具未显示的问题。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "用 MCP 连接工具",
  lede:
    "通过 Model Context Protocol 连接本地工具服务器或远程服务。Codewhale 会发现服务器提供的工具，并在使用时执行你的工具权限和审批规则。",
  sections: [
    {
      id: "add",
      title: "试用 Cloudflare Docs 服务器",
      blocks: [
        {
          p: "公开的 [Cloudflare 文档服务器](https://developers.cloudflare.com/agents/model-context-protocol/cloudflare/servers-for-cloudflare/)提供文档搜索和 Pages 迁移到 Workers 的指南。[查看官方源码](https://github.com/cloudflare/mcp-server-cloudflare/tree/main/apps/docs-vectorize)。无需凭据即可添加：",
        },
        {
          code: `codewhale mcp add cloudflare-docs --url "https://docs.mcp.cloudflare.com/mcp"
codewhale mcp tools cloudflare-docs`,
          lang: "终端",
        },
        {
          p: "工具列表应包含 `search_cloudflare_documentation` 和 `migrate_pages_to_workers_guide`。如果 TUI 会话已在运行，请在其中执行 `/mcp reload`，把新配置加载到该会话。",
        },
        {
          note: "2026 年 11 月 7 日使用已发布的 CLI v0.10.2 检查：两条命令均成功退出，无需凭据即可列出上述两个工具。这验证了配置和发现流程，没有执行工具。[发布版指南](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MCP.md)对应此版本；[main 源码指南](https://github.com/codewhale-hq/Codewhale/blob/main/docs/MCP.md)可能包含尚未发布的变更。",
        },
        {
          p: "在会话中，`/mcp` 会打开 MCP 管理器：显示每个服务器的状态、传输方式、超时设置、错误信息和已发现的工具。同样的操作在那里也能完成，例如 `/mcp add stdio <name> <command>` 和 `/mcp add http <name> <url>`。",
        },
        {
          note: "本地 MCP 服务器以 Codewhale 进程的权限运行；远程调用使用你授予该服务的访问权限。只添加你信任的服务器。",
        },
      ],
    },
    {
      id: "compatibility",
      title: "可以连接哪些服务器",
      blocks: [
        {
          rows: [
            ["本地 stdio", "Codewhale 启动 `--command` 指定的程序，每个参数用一个 `--arg`。请先安装服务器及其所需运行时。"],
            ["远程 HTTP / SSE", "`--url` 使用 Streamable HTTP，并可回退到旧版 SSE。旧版 SSE 端点可设置 `--transport sse`。远程配置不需要 `command`。"],
            ["直接配置的远程认证", "你自己的 MCP 配置可以使用环境变量请求头、bearer token 或 OAuth。OAuth 需要服务器提供兼容的认证元数据和客户端注册方式。"],
            ["插件提供的 MCP", "经过审查并启用的插件可以提供 stdio 或远程 MCP 服务器。插件 MCP 支持基于环境变量的认证，但不支持插件提供的 MCP OAuth。"],
            ["工具、资源与提示词", "Codewhale 发现服务器实际声明的能力。只有服务器实现了资源、模板或提示词功能，才能使用这些功能。"],
            ["可选扩展与 MCP Apps", "尚不提供通用的 MCP Apps 界面支持。支持基本 MCP 传输不等于兼容所有可选扩展或服务器。"],
            ["服务器要求", "每个服务器都有自己的操作系统、程序、浏览器、依赖和账号要求。本地服务器运行在承载 Codewhale 的电脑上；Computer Use 有其独立的平台限制。"],
          ],
        },
        {
          p: "直接添加 MCP 不需要插件。[插件](/plugins)可以把 MCP 工具、指令和其他组件打包在一起；各组件仍需要兼容的依赖、审查和权限。",
        },
      ],
    },
    {
      id: "remote-auth",
      title: "登录远程服务器",
      blocks: [
        {
          p: "直接配置 OAuth 服务器时，先添加它的真实 URL，再用你设置的服务器名称登录：",
        },
        {
          code: `codewhale mcp login <server>
codewhale mcp logout <server>`,
          lang: "终端",
        },
        {
          p: "使用 bearer token 时，添加服务器时指定 `--bearer-token-env-var TOKEN_NAME`，并在启动 Codewhale 的环境中设置该变量。`env_headers` 把请求头名称映射到环境变量名。字面量 `headers` 不会展开 `${TOKEN_NAME}`。插件 MCP 不能使用 OAuth；需要 OAuth 时，请直接配置该服务器。",
        },
        {
          p: "显式设置的 Authorization 请求头始终优先：先应用配置中的请求头，其次是 bearer token 环境变量，最后才是已保存的 OAuth 登录。`codewhale mcp logout <name>` 会删除本机保存的登录信息；提供商那边的授权可能仍然有效，需要到提供商处撤销。",
        },
      ],
    },
    {
      id: "config",
      title: "编辑配置文件",
      blocks: [
        {
          p: "全局服务器配置保存在 `~/.codewhale/mcp.json`；如果该文件不存在，仍会读取旧版 `~/.deepseek/mcp.json`。`codewhale mcp init` 会生成初始文件。可用绝对路径 `mcp_config_path` 或 `CODEWHALE_MCP_CONFIG` 覆盖默认位置。环境变量也接受 `DEEPSEEK_MCP_CONFIG`；两者都设置时，Codewhale 名称优先。",
        },
        {
          code: `{
  "servers": {
    "cloudflare-docs": {
      "url": "https://docs.mcp.cloudflare.com/mcp"
    }
  }
}`,
          lang: "mcp.json",
        },
        {
          p: "`servers` 和别名 `mcpServers` 二选一，不要在同一文件中同时使用。`env_http_headers` 是 `env_headers` 的别名。请按服务器文档调整命令、参数和认证字段，不要假定所有客户端的配置格式都能直接互换。",
        },
        {
          p: "受信任的工作区可以提供 `.codewhale/mcp.json`，其中同名服务器会覆盖全局配置；仓库不能自行授予信任，也不能覆盖你的全局配置路径。全局或受信任项目中的显式配置，优先于标识冲突的插件服务器。",
        },
        {
          p: "修改后在 TUI 中运行 `/mcp reload` 即可，无需重启。默认情况下，已启用且获准的服务器会在会话启动时连接。用户配置的服务器可设置 `\"lazy\": true` 按需连接；设置 `\"required\": true` 后仍会在启动时连接。插件 MCP 清单没有 lazy 选项。无界面的运行时在配置变更后需要重启。",
        },
      ],
    },
    {
      id: "tool-names",
      title: "找到这些工具",
      blocks: [
        {
          p: "每个工具在模型眼中的名字是 `mcp_<server>_<tool>`。本例中的文档搜索是 `mcp_cloudflare-docs_search_cloudflare_documentation`。`codewhale mcp tools <server>` 在自己的进程中列出发现的工具，不会把连接附加到正在运行的会话。",
        },
        {
          p: "MCP 工具遵循你的[审批设置](/docs/modes)、允许/拒绝规则及网络策略。调用前是否询问，取决于策略与权限姿态。Full Access 不会绕过仓库规则或托管策略。",
        },
      ],
    },
    {
      id: "troubleshooting",
      title: "工具没有出现时",
      blocks: [
        {
          rows: [
            ["检查配置", "`codewhale doctor` 提供静态路径和配置诊断。终端中的 `codewhale mcp validate` 会在独立进程中连接已启用且获准的服务器，属于实际连接检查。"],
            ["刷新当前会话", "TUI 中的 `/mcp validate` 只显示上次快照，不会重新连接。配置变更后使用 `/mcp reload`；单个服务器可用 `/mcp retry <name>`。"],
            ["已连接但缺少工具", "检查 `codewhale mcp tools <server>`、服务器声明的工具、`enabled_tools` / `disabled_tools`，以及会话的允许/拒绝规则。传输已连接不代表某个工具可用或已获准。"],
            ["需要认证", "直接配置的 OAuth 服务器可运行 `/mcp login <name>`。令牌认证请检查变量是否存在于 Codewhale 的启动环境中。在一个进程中登录成功，不会把该进程的连接附加到另一个进程。"],
            ["本地命令失败", "检查服务器程序、参数、工作目录和运行时。Windows 上的包运行器可能需要使用 `.cmd` 名称。请遵循服务器提供方的操作系统和依赖说明。"],
            ["插件服务器缺失", "检查所属插件的审查、信任和启用状态。MCP 设置不能激活不受信任的插件，也不能绕过被拒绝的清单。"],
          ],
        },
      ],
    },
    {
      id: "serve",
      title: "把 Codewhale 作为 MCP 服务器运行",
      blocks: [
        {
          p: "支持 Codewhale 声明的 MCP 协议版本的客户端，可以把它作为 stdio 工具服务器启动。默认仅暴露只读的 `file_read` 和 `search` 工具。",
        },
        {
          code: `codewhale mcp add-self
codewhale mcp tools codewhale`,
          lang: "终端",
        },
        {
          p: "`add-self` 注册 `codewhale serve --mcp`；`codewhale mcp-server` 是等价入口。这个服务器从旧版路径 `~/.deepseek/mcp_server.toml` 读取独立的 `[server]` 设置，而不是客户端的 `mcp.json`。写入或 shell 工具需要显式列出，并设置 `require_approval = false`；这里没有交互式审批通道。[Runtime API](/docs/runtime-api) HTTP 与 MCP 分开。",
        },
      ],
    },
    {
      id: "code-mode",
      title: "用代码模式组合工具调用（实验性）",
      blocks: [
        {
          note: "本节依据 main 候选版本的源码，可能包含比已发布 v0.10.2 更新的行为。",
        },
        {
          p: "代码模式让模型写一段简短的 JavaScript 程序，在其中调用多个工具、循环和筛选结果，而不必把每次调用都作为单独的一步。只有程序的最终结果会返回给模型，长时间的查找因此更紧凑。它默认开启——`execute_tools` 从第一条请求起就会提供——MCP 和插件工具与内置工具一样可以在程序中组合调用。关闭它并不会移除代码模式：`execute_tools` 会恢复为延迟加载，模型仍可通过 `tool_search` 找到它。要关闭它，可以只在一次会话中关闭，也可以在配置中关闭：",
        },
        {
          code: `codewhale --disable code_mode

# ~/.codewhale/config.toml
[features]
code_mode = false`,
          lang: "终端 / config.toml",
        },
        {
          list: [
            "程序中的嵌套调用——原生工具、MCP 和插件工具都一样——与直接调用走同一道关卡：允许/拒绝名单、ask 规则和审查照常生效。需要审批的调用会弹出常规审批卡片并暂停程序；允许后程序继续，拒绝只让该次调用失败。批准程序本身不会获得任何权限。",
            "MCP 调用经由会话的 MCP 连接池执行，插件工具也可以像内置工具一样在程序中调用。",
            "每个程序的上限：50 次嵌套调用、同时最多 4 次、返回 16 KiB。程序没有固定的 30 秒上限：它使用当前回合剩余的墙钟时间，除非设置了 `[tui].turn_wall_clock_secs`，否则不受限制。",
            "Plan 模式下不能使用代码模式。",
          ],
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/hooks",
      label: "在事件发生时运行命令",
      note: "在工具调用执行前检查或改写它，MCP 工具也不例外。",
    },
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "决定哪些 MCP 调用需要你批准。",
    },
    {
      href: "/docs/runtime-api",
      label: "用 Runtime API 自动化",
      note: "通过 HTTP 从你自己的应用或脚本驱动 Codewhale。",
    },
  ],
  sourceNote:
    "来源文档：docs/MCP.md、crates/engine/src/tools/codemode.rs · 修改时同步更新 docs-map.ts。",
};
