import type { DocsRuntimeApiDict } from "../types";

/** 「用 Runtime API 自动化」页的简体中文词典；与 `en/docs-runtime-api.ts` 逐段对应。 */
export const docsRuntimeApi: DocsRuntimeApiDict = {
  metaTitle: "用 Runtime API 自动化 · Codewhale 文档",
  metaDescription:
    "用你自己的脚本和应用驱动 Codewhale：在 CI 中运行一次性任务，或启动本地 HTTP API 来发送回合、接收事件流并回应审批。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "用 Runtime API 自动化",
  lede:
    "单次任务可使用 `codewhale exec`；本教程演示如何通过本地 Runtime API 创建线程并读取回合事件流。[完整参考](/docs/runtime-reference)（由仓库 main 分支生成）介绍路由、请求字段、事件和兼容性限制。",
  sections: [
    {
      id: "exec",
      title: "在脚本中运行单个任务",
      blocks: [
        {
          code: `codewhale exec "Reply with exactly: pong"
codewhale exec --auto --sandbox workspace-write "fix the failing test and run it again"
codewhale exec --auto --sandbox workspace-write --output-format stream-json "update the changelog"`,
          lang: "终端",
        },
        {
          p: "普通的 `exec` 只回答一次，不使用工具。`--auto` 默认启用 Full Access（完全访问）：工具无需批准即可运行，shell 命令没有 Codewhale 沙箱，信任模式允许文件工具写入工作区以外的路径。除 `danger-full-access` 外的任何 `--sandbox`（或配置中对应的 `sandbox_mode`）都会保持信任模式关闭。加上 `--sandbox workspace-write` 可在可用时启用 [shell 沙箱](/docs/sandbox)，并保留文件工具的工作区限制；`external-sandbox` 则依赖你自行提供的隔离。请把标志放在提示词前；提示词以 `-` 开头或包含需要按字面理解的 exec 标志时，应先加上 `--`。向模型提供工具的运行会在所有输出格式下保存。`--output-format stream-json` 每行输出一个 JSON 事件，也会保存仅作答的运行，以便之后用 `--continue` 接着运行。`--max-turns` 和 `--allowed-tools` 可以为一次运行设定限制。",
        },
      ],
    },
    {
      id: "start",
      title: "启动 Runtime API",
      blocks: [
        { p: "请使用具备 `curl`、`jq` 和 `openssl` 的 POSIX shell，以及支持 `app-server --http` 的 Codewhale 构建。在同一个终端中完成所有步骤，才能保留令牌、服务进程 id 和线程 id。" },
        { note: "本页的命令均存在于已发布版本；请先运行 `codewhale --version` 核对版本。下方回合需要已连接的提供商，并按其正常规则计费；启动服务和检查 `/health` 不发送模型请求。" },
        {
          code: `export CODEWHALE_RUNTIME_TOKEN="$(openssl rand -hex 32)"
codewhale app-server --http &
CODEWHALE_RUNTIME_PID=$!
curl --fail --silent --show-error --retry 10 --retry-connrefused \  --retry-delay 1 http://127.0.0.1:7878/health`,
          lang: "终端",
        },
        {
          p: "健康检查成功后再继续；如果失败或端口被占用，请查看服务输出。请妥善保管生成的令牌，每个 `/v1/*` 请求都必须以 `Authorization: Bearer <token>` 的形式携带它。",
        },
      ],
    },
    {
      id: "turn",
      title: "发送一个回合并观察它",
      blocks: [
        {
          code: `API=http://127.0.0.1:7878
AUTH="Authorization: Bearer $CODEWHALE_RUNTIME_TOKEN"

THREAD=$(curl -fsS -X POST "$API/v1/threads" -H "$AUTH" \\
  -H "Content-Type: application/json" -d '{}' | jq -er .id)

curl -fsS -X POST "$API/v1/threads/$THREAD/turns" -H "$AUTH" \\
  -H "Content-Type: application/json" -d '{"prompt": "Summarize README.md"}'

curl -fN "$API/v1/threads/$THREAD/events?since_seq=0" -H "$AUTH"`,
          lang: "终端",
        },
        {
          p: "请确认创建线程后返回非空 id，回合请求返回已接受的回合，事件流随后应显示回合及其记录的结果。重新连接时保留最后接受的 `seq`，处理终止帧 `stream.end`，并遵循[回放约定](/docs/runtime-reference#ending-and-resuming-a-thread-stream)，不要假定所有断线都能恢复。",
        },
        {
          rows: [
            ["停止回合", "`POST /v1/threads/{id}/turns/{turn_id}/interrupt`"],
            ["回应审批", "`POST /v1/approvals/{approval_id}`"],
            ["引导正在运行的回合", "`POST /v1/threads/{id}/turns/{turn_id}/steer`"],
            ["列出已保存的会话", "`GET /v1/sessions`"],
          ],
        },
        {
          p: "[Runtime API 参考](/docs/runtime-reference) 展示仓库 main 分支的契约，包括端点表、请求字段、事件封装和版本限制。",
        },
      ],
    },
    {
      id: "stop",
      title: "停止教程服务",
      blocks: [
        { p: "按 Ctrl+C 关闭事件流，然后停止你在此终端启动的服务。仅关闭事件流不会停止服务，也不会中断正在运行的模型回合；需要时请使用上方的中断端点。" },
        { code: 'kill "$CODEWHALE_RUNTIME_PID"\nunset CODEWHALE_RUNTIME_TOKEN CODEWHALE_RUNTIME_PID', lang: "终端" },
      ],
    },
    {
      id: "other",
      title: "选择其他连接方式",
      blocks: [
        {
          rows: [
            ["codewhale app-server --stdio", "通过标准输入输出传输 JSON-RPC，不监听网络。适合 SDK 或本地探测。"],
            ["codewhale serve --acp", "面向 Zed 等编辑器的 Agent Client Protocol。"],
            ["codewhale serve --mcp", "把 Codewhale 的工具提供给其他 MCP 客户端。见[用 MCP 连接工具](/docs/mcp)。"],
            ["codewhale web", "内置的[浏览器客户端](/docs/web)，基于同一个 API。"],
            ["codewhale doctor --json", "以 JSON 输出健康状况和能力信息，不含任何密钥。"],
          ],
          codeTerms: true,
        },
      ],
    },
    {
      id: "security",
      title: "保持私密",
      blocks: [
        {
          list: [
            "服务默认只监听 `127.0.0.1`。令牌只是本地防护，不能替代 TLS 或 VPN；不要把端口暴露到网络上。",
            "`--insecure-no-auth` 只在回环地址上被接受。",
            "API 从不返回你的提供商密钥。健康和能力报告只包含元数据——没有密钥、文件内容或消息。",
          ],
        },
      ],
    },
  ],
  next: [
    { href: "/docs/runtime-reference", label: "阅读 Runtime API 参考", note: "查找准确的路由、字段、事件结构和兼容性行为。" },
    {
      href: "/docs/web",
      label: "打开浏览器客户端",
      note: "基于同一个 API 的现成客户端。",
    },
    {
      href: "/docs/hooks",
      label: "在事件发生时运行命令",
      note: "不写客户端，也能响应会话事件。",
    },
    {
      href: "/docs/fleet",
      label: "运行工作流",
      note: "可在任何终端查看的持久多步骤运行。",
    },
  ],
  sourceNote: "来源文档：docs/RUNTIME_API.md · 修改时同步更新 docs-map.ts。",
};
