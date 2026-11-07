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
    { title: "连接智能体", body: "在 Codewhale 中运行 /plugin marketplace install codewhale computer-use，审阅并启用插件。其他兼容 MCP 的宿主可按设置指南连接助手。" },
  ],
  controlsTitle: "随时掌握控制权",
  controlsBody: "授予 macOS 权限后，标准模式无需逐个应用询问。你禁用的应用仍不可访问；Pause 暂停电脑操作，Stop computer use 结束当前会话。也可选择严格审批模式。想观看操作过程时，再请求打开实时预览。",
  updateTitle: "由你决定何时更新",
  updateBody: "在应用中选择 Check for updates。安装更新前，助手会校验下载文件、Codewhale 签名和 Apple 公证，并保留旧版应用以便恢复。",
  help: "设置与故障排查",
  notes: "版本说明",
  demo: "查看后台检查演示",
  source: "其他宿主与平台",
  platforms: "Mac 助手为测试版，市场插件支持 Mac。已发布版本另含未签名的 Windows x64 实验性预览包；Linux 需要实验性源码或 Docker 设置。Windows 和 Linux 的输入使用共享桌面。",
};
