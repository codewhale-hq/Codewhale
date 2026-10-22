import type { LocalizedText } from "./vocabulary";

export const TOOLS_COPY = {
  metaTitle: { en: "Built-in tools · Codewhale Docs", zh: "内置工具 · Codewhale 文档" },
  metaDescription: { en: "Core work tools, on-demand discovery, and exact replay compatibility.", zh: "核心工作工具、按需发现与精确回放兼容。" },
  title: { en: "Built-in tools", zh: "内置工具" },
  lead: { en: "Every session starts with a small set of tools for reading, editing, running commands, and delegating. Codewhale loads specialized tools only when a task needs them, and your mode and approval setting still apply to all of them.", zh: "每个会话一开始就带着一小组工具，用于阅读、修改、运行命令和委派任务。专用工具只在任务需要时才加载，而你的模式和审批设置对所有工具都同样有效。" },
  reference: { en: "Tool contract and design rationale", zh: "工具约定与设计说明" },
  rows: [
    { name: "read", detail: { en: "path · offset? · limit?", zh: "path · offset? · limit?" } },
    { name: "write", detail: { en: "path · content", zh: "path · content" } },
    { name: "edit", detail: { en: "path · edits", zh: "path · edits" } },
    { name: "file_search", detail: { en: "Find workspace files by name, path, or glob pattern.", zh: "按文件名、路径或 glob 通配模式查找工作区文件。" } },
    { name: "bash", detail: { en: "command · timeout?", zh: "command · timeout?" } },
    { name: "agent · workflow", detail: { en: "Loaded on demand via tool_search. Delegate work and coordinate dependent phases.", zh: "通过 tool_search 按需加载。委派工作并协调有依赖关系的阶段。" } },
    { name: "todo_write", detail: { en: "Replace the task list with content and status entries.", zh: "用包含内容与状态的条目替换任务列表。" } },
    { name: "create_goal · get_goal · update_goal", detail: { en: "create_goal is available without discovery; get_goal/update_goal load on demand while a goal is active.", zh: "create_goal 无需搜索即可使用；get_goal/update_goal 仅在目标激活时按需加载。" } },
    { name: "task_shell_wait", detail: { en: "Poll or stop a background shell task by task_id.", zh: "按 task_id 轮询或停止后台 shell 任务。" } },
    { name: "load_skill", detail: { en: "Load a skill by exact name, or pass query to search the catalogue.", zh: "按确切名称加载技能，或传入 query 搜索技能目录。" } },
    { name: "tool_search", detail: { en: "Discover policy-allowed tools. Each child has its own search and activation cache.", zh: "发现策略允许的工具。每个子 Agent 拥有独立的搜索与激活缓存。" } },
    { name: "Git · Run · tasks · remember · Web · MCP · plugins", detail: { en: "Deferred: loaded by tool_search only when policy permits.", zh: "延迟加载：仅在策略允许时由 tool_search 加载。" } },
    { name: "8 names / 24 KiB", detail: { en: "Conversation toolbox cache; independent per child and revalidated every turn.", zh: "会话工具箱缓存；每个子 Agent 独立，每轮重新校验。" } },
    { name: "Web search/fetch", detail: { en: "Scout and Reviewer children can discover these without gaining mutation or arbitrary network authority.", zh: "侦察与审查子 Agent 可发现这些工具，但不会因此获得写入或任意网络权限。" } },
    { name: "MCP", detail: { en: "mcp_<server>_<tool> — registered from ~/.codewhale/mcp.json.", zh: "mcp_<server>_<tool>——从 ~/.codewhale/mcp.json 注册。" } },
  ],
  compatibilityTitle: { en: "Replay compatibility", zh: "回放兼容" },
  lspTitle: { en: "Read language-server diagnostics", zh: "读取语言服务器诊断" },
  lspBody: { en: "After file edits, LSP diagnostics can report errors to the agent. Add a custom server by file extension under [lsp.custom]; install the server executable separately.", zh: "文件修改后，LSP 诊断可向智能体报告错误。通过 [lsp.custom] 按扩展名添加自定义服务器；服务器可执行文件需要单独安装。" },
  lspDocs: { en: "PHP and custom LSP server setup", zh: "PHP 与自定义 LSP 服务器设置" },
  compatibility: { en: "Legacy names remain only for saved transcripts and protocol clients. Exact old calls retain their handlers but stay out of new catalogs and tool_search. Unknown names are never guessed or rewritten.", zh: "旧名称仅为已保存的记录与协议客户端保留。精确旧调用仍使用原处理器，但不会出现在新目录或 tool_search 中。未知名称不会被猜测或改写。" },
} satisfies {
  metaTitle: LocalizedText;
  metaDescription: LocalizedText;
  title: LocalizedText;
  lead: LocalizedText;
  reference: LocalizedText;
  rows: { name: string; detail: LocalizedText }[];
  compatibilityTitle: LocalizedText;
  lspTitle: LocalizedText;
  lspBody: LocalizedText;
  lspDocs: LocalizedText;
  compatibility: LocalizedText;
};
