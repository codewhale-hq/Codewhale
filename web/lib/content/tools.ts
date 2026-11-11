import type { CapabilityClaim, CapabilityContext } from "../capabilities";
import { showsReleasedText, sourcePlacement } from "../docs-release";
import { FEATURE_CATALOGUE, type Feature } from "../features";
import type { LocalizedText } from "./vocabulary";

export interface ToolRow {
  name: string;
  detail: LocalizedText;
  until?: CapabilityClaim;
  capability?: CapabilityClaim;
}

export const EAGER_SET_CLAIM = { feature: "present-handoff" } as const;

export const TOOLS_COPY = {
  metaTitle: { en: "Built-in tools · Codewhale Docs", zh: "内置工具 · Codewhale 文档" },
  metaDescription: { en: "Core work tools, on-demand discovery, and exact replay compatibility.", zh: "核心工作工具、按需发现与精确回放兼容。" },
  title: { en: "Built-in tools", zh: "内置工具" },
  lead: { en: "In the published release, every session starts with thirteen native tools for reading, editing, running commands, and delegating, plus tool_search. Codewhale loads specialized tools only when a task needs them, and your mode and approval setting still apply to all of them.", zh: "在已发布版本中，每个会话一开始就带着十三个原生工具，用于阅读、修改、运行命令和委派任务，外加 tool_search。专用工具只在任务需要时才加载，而你的模式和审批设置对所有工具都同样有效。" },
  leadSource: { en: "Every session starts with fourteen native tools for reading, editing, running commands, and delegating, plus tool_search.", zh: "每个会话一开始就带着十四个原生工具，用于阅读、修改、运行命令和委派任务，外加 tool_search。" },
  reference: { en: "Tool contract and design rationale", zh: "工具约定与设计说明" },
  rows: [
    { name: "read", detail: { en: "path · offset? · limit? · expected_revision?", zh: "path · offset? · limit? · expected_revision?" } },
    { name: "write", detail: { en: "path · content", zh: "path · content" } },
    { name: "edit", detail: { en: "path · edits", zh: "path · edits" } },
    { name: "file_search", detail: { en: "Find workspace files by name, path, or glob pattern.", zh: "按文件名、路径或 glob 通配模式查找工作区文件。" } },
    { name: "bash", detail: { en: "command · timeout?", zh: "command · timeout?" } },
    { name: "agent", detail: { en: "Delegate focused work without a discovery hop, subject to child-depth authority.", zh: "无需搜索即可委派专注任务，仍受子 Agent 深度权限约束。" } },
    { name: "todo_write", detail: { en: "Replace the task list with content and status entries.", zh: "用包含内容与状态的条目替换任务列表。" } },
    { name: "create_goal · get_goal · update_goal", detail: { en: "Parent goal controls are available without discovery. Children may inspect but not mutate the root goal.", zh: "父 Agent 的目标控制无需搜索即可使用；子 Agent 可查看但不可修改根目标。" } },
    { name: "task_shell_wait", detail: { en: "Poll or stop a background shell task by task_id.", zh: "按 task_id 轮询或停止后台 shell 任务。" } },
    { name: "load_skill", detail: { en: "Load a skill by exact name, or pass query to search the catalogue.", zh: "按确切名称加载技能，或传入 query 搜索技能目录。" } },
    { name: "tool_search", detail: { en: "Discover policy-allowed tools. Each child has its own search and activation cache.", zh: "发现策略允许的工具。每个子 Agent 拥有独立的搜索与激活缓存。" } },
    { name: "workflow", detail: { en: "Plan, run and inspect multi-agent phases with dependencies and completion checks.", zh: "规划、运行并查看带依赖与完成检查的多智能体阶段。" }, until: EAGER_SET_CLAIM },
    { name: "Git · Run · tasks · remember · Web · MCP · plugins", detail: { en: "Deferred: loaded by tool_search only when policy permits.", zh: "延迟加载：仅在策略允许时由 tool_search 加载。" }, until: EAGER_SET_CLAIM },
    { name: "grep_files", detail: { en: "Search file contents directly; focused Rust matches can include snapshot-guarded source references.", zh: "直接搜索文件内容；聚焦的 Rust 匹配可附带版本守卫源码引用。" }, capability: EAGER_SET_CLAIM },
    { name: "present", detail: { en: "Hand off 1–32 existing files with SHA-256 receipts; read-only.", zh: "以 SHA-256 回执交付 1–32 个现有文件；只读。" }, capability: EAGER_SET_CLAIM },
    { name: "workflow · context_archive_search · context_archive_get · Git · Run · tasks · remember · Web · MCP · plugins", detail: { en: "Deferred: loaded by tool_search only when policy permits. context_archive_search and context_archive_get search and read compacted history, with credentials masked.", zh: "延迟加载：仅在策略允许时由 tool_search 加载。context_archive_search 与 context_archive_get 用于搜索并读取被压缩替换的历史内容，凭据会被遮蔽。" }, capability: EAGER_SET_CLAIM },
    { name: "8 names / 16 KiB", detail: { en: "Conversation toolbox cache; independent per child and revalidated every turn.", zh: "会话工具箱缓存；每个子 Agent 独立，每轮重新校验。" } },
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
  leadSource: LocalizedText;
  rows: ToolRow[];
  compatibilityTitle: LocalizedText;
  lspTitle: LocalizedText;
  lspBody: LocalizedText;
  lspDocs: LocalizedText;
  compatibility: LocalizedText;
};

export function toolRowsFor(context: CapabilityContext, features: readonly Feature[] = FEATURE_CATALOGUE.features): { body: ToolRow[]; source: ToolRow[]; released: boolean } {
  const placement = (row: ToolRow) => (row.capability ? sourcePlacement(row.capability, context, features) : "body");
  return {
    body: TOOLS_COPY.rows.filter((row) => (row.capability ? placement(row) === "body" : showsReleasedText(row.until, context, features))),
    source: TOOLS_COPY.rows.filter((row) => row.capability && placement(row) === "source"),
    released: showsReleasedText(EAGER_SET_CLAIM, context, features),
  };
}
