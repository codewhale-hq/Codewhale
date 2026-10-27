import type { ComputerUseDict } from "../types";

/**
 * Simplified-Chinese dictionary for `app/[locale]/computer-use/page.tsx` and
 * the Computer Use section of the install page. Product names, the menu
 * items Pause, Stop and Check for updates, and the macOS setting names stay
 * as the app shows them.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · 预览 · Codewhale",
  metaDescription: "预览功能：下载并设置 Mac 版 Codewhale Computer Use。提供应用后台操作、权限设置，以及由用户控制的暂停和停止功能。",
  title: "Computer Use",
  lead: "你继续自己的工作，让 Codewhale 在各个应用间帮你操作。在这台 Mac 上授权一次，随时都能停止电脑操作。",
  publisher: "由 Codewhale 开发",
  download: "下载 Mac 版",
  downloadZip: "ZIP 压缩包（供应用内更新使用）",
  requirements: "macOS 13.5 或更高版本 · Apple 芯片与 Intel",
  included: "只需下载一个应用，无需另行安装 Node 或编译器。",
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
    { title: "启用 Computer Use", body: "在 Codewhale 的 Connectors 设置中安装或更新 Computer Use，然后审查并启用它。请它在应用中执行任务。首次使用时，在原生助手中选择 Trust until revoked；之后切换应用、模型或任务时，这份信任都会继续有效。" },
  ],
  controlsTitle: "随时掌握控制权",
  controlsBody: "操作目标应用旁会显示小巧的 Codewhale 控件。Pause 暂停电脑操作，Stop computer use 结束所有当前电脑会话。在 Settings 的 Remembered access 中选择 Turn off，即可撤销 Codewhale 的访问权限。想观看操作过程时，再请求打开实时预览。",
  updateTitle: "由你决定何时更新",
  updateBody: "在应用中选择 Check for updates。安装更新前，助手会校验下载文件、Codewhale 签名和 Apple 公证，并保留旧版应用以便恢复。",
  help: "设置与故障排查",
  notes: "版本说明",
  demo: "查看后台检查演示",
  source: "源码与其他平台",
  platforms: "此下载适用于 Mac。Windows 和 Linux 目前通过源码插件及宿主端设置使用。",
};
