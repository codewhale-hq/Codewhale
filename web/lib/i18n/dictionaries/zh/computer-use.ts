import type { ComputerUseDict } from "../types";

/**
 * Simplified-Chinese dictionary for `app/[locale]/computer-use/page.tsx` and
 * the Computer Use section of the install page. Product names, the menu
 * items Pause, Stop and Check for updates, and the macOS setting names stay
 * as the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Mac 测试版 · Codewhale",
  metaDescription: "智能体在 Mac 应用中后台操作，你可以继续使用电脑。下载经过 Apple 公证的 Computer Use 助手，并连接兼容的 MCP 宿主。",
  title: "Computer Use",
  lead: "让智能体在 Mac 应用中工作，你可以继续使用电脑。",
  publisher: "由 Codewhale 开发",
  download: "下载 Mac 版",
  downloadZip: "ZIP 压缩包（供应用内更新使用）",
  requirements: "macOS 13.5 或更高版本 · Apple 芯片与 Intel",
  included: "Mac 助手自带运行环境。请在 MCP 宿主中连接 Computer Use；宿主需要单独设置。",
  pendingTitle: "Mac 下载包准备中",
  pendingBody: "完成 Apple 公证和发布检查后，正式安装包将在此提供。",
  unavailableTitle: "暂时无法查询下载状态",
  unavailableBody: "请刷新页面重试，或通过下方链接查看已发布版本。",
  releases: "已发布版本",
  receipt: "下载校验信息",
  setup: "设置你的 Mac",
  steps: [
    { title: "安装应用", body: "打开下载的磁盘映像，将 Codewhale Computer Use 拖入“应用程序”。从“应用程序”中打开它，然后点击菜单栏中的鲸鱼图标，选择 Computer Use。" },
    { title: "检查权限", body: "通过设置按钮打开系统设置中的“辅助功能”和“屏幕录制”。由你决定授予哪些权限。" },
    { title: "连接智能体", body: "选择下方适用于你的 CLI 版本的 Codewhale 设置方式。其他兼容 MCP 的宿主请参阅仓库中的设置指南。" },
  ],
  controlsTitle: "随时掌握控制权",
  controlsBody: "授予 macOS 权限后，标准模式无需逐个应用询问。你禁用的应用仍不可访问；Pause 暂停电脑操作，Stop computer use 结束当前会话。也可选择严格审批模式。想观看操作过程时，再请求打开实时预览。",
  updateTitle: "由你决定何时更新",
  updateBody: "在应用中选择 Check for updates。安装更新前，助手会校验下载文件、Codewhale 签名和 Apple 公证，并保留旧版应用以便恢复。",
  help: "设置与故障排查",
  notes: "版本说明",
  demo: "查看后台检查演示",
  source: "在 GitHub 上查看",
  platforms: "Mac 助手为测试版，市场插件支持 Mac。已发布版本另含未签名的 Windows x64 实验性预览包；Linux 需要实验性源码或 Docker 设置。Windows 和 Linux 的输入使用共享桌面。",
  connectTitle: "连接 Codewhale",
  connectVersion: "Codewhale 0.10.2 内置较旧的 Computer Use 插件，安装目录中的同名条目不会将其升级。请通过 MCP 直接连接已安装的 Mac 助手。",
  connectLimit: "直接连接 MCP 只添加工具，不包含插件技能。请勿与旧版内置插件同时使用；如果该插件已启用，请先运行 /plugin disable computer-use。",
  connectRuntime: "应用自带 Node，无需另行安装。如果应用安装在其他位置，请将 APP 设为实际应用包路径。",
  connectReload: "在正在运行的终端会话中执行 /mcp reload，然后输入：",
  connectPrompt: "使用 request_access 检查 Computer Use 权限。不要发送任何输入。",
  connectExpected: "检查回复中的 via: app、助手版本及缺少的权限。如果找不到助手，请停止并按设置提示操作。此步骤仅检查连接和权限。",
  connectGuide: "MCP 设置指南",
  contributeTitle: "一起改进 Computer Use",
  contributeBody: "完善文档，或测试应用操作流程并分享可复现的结果。",
  contributeGuide: "贡献指南",
  contributeIssue: "报告问题",
};
