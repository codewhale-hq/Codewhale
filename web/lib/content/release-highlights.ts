export const RELEASE_HIGHLIGHTS = {
  version: "0.10.3",
  comingHeading: { en: "Coming in v{version}", zh: "v{version} 即将推出" },
  newHeading: { en: "New in v{version}", zh: "v{version} 新功能" },
  comingLede: { en: "Merged on main and not in a published release yet. Build main to try it now.", zh: "已合并到 main，尚未包含在已发布版本中。现在可从 main 构建试用。" },
  changelogLink: { en: "Read the changelog", zh: "查看更新日志" },
  items: [
    { title: "MCP", body: { en: "MCP servers now connect through the official MCP SDK (2.4) behind Codewhale's Rust broker. Newer servers are discovered with server/discover; older stdio and HTTP servers fall back to the legacy handshake.", zh: "MCP 服务器现在通过官方 MCP SDK（2.4）连接，由 Codewhale 的 Rust 代理把关。较新的服务器通过 server/discover 发现；较旧的 stdio 和 HTTP 服务器回退到旧版握手。" } },
    { title: "MCP fixes", body: { en: "Older servers that reject the discovery probe still connect. A server that refuses sign-in shows its real 401 and the /mcp login hint, and one whose command cannot start says why.", zh: "拒绝发现探测的旧服务器仍可连接。拒绝登录的服务器会显示真实的 401 和 /mcp login 提示；命令无法启动的服务器会说明原因。" } },
    { title: "DeepSeek Harness plugins", body: { en: "Reviewed DSH plugins run in the Native extension host. Tool output is checked against its declared schema, and unsupported options are refused instead of ignored.", zh: "经过审核的 DSH 插件在 Native 扩展宿主中运行。工具输出按声明的 schema 校验，不支持的选项会被拒绝而不是被忽略。" } },
    { title: "Computer Use", body: { en: "No longer bundled in the binary. Install it from the codewhale marketplace, which points at its own release; every decision still needs your approval.", zh: "不再内置于二进制文件中。可从 codewhale 插件市场安装，市场指向它的独立发布版本；每个决定仍需你批准。" } },
    { title: "Image-heavy sessions", body: { en: "Older images are left out of an outbound request with a receipt, and an HTTP 413 is retried once with a smaller body. Saved images stay intact.", zh: "较早的图片会从发出的请求中省略并留下记录；遇到 HTTP 413 时会用更小的请求体重试一次。已保存的图片保持不变。" } },
    { title: "Instruction budgets", body: { en: "Global and project instructions get separate budgets, and every trim, skip or refusal shows in /context report.", zh: "全局指令和项目指令分别拥有各自的预算，每次截断、跳过或拒绝都会显示在 /context report 中。" } },
    { title: "codewhale uninstall", body: { en: "On Unix, previews the release files the installer recorded; --yes removes only those. Your data and package-managed installs are kept.", zh: "在 Unix 上会预览安装程序记录的发布文件；--yes 只删除这些文件。你的数据和包管理器安装的版本会保留。" } },
    { title: "/rc", body: { en: "codewhale rc attaches to the hosted control plane for web Chat again: it accepts the exact released runtime identity instead of only the retired 0.9.13 one.", zh: "codewhale rc 可以再次连接托管控制平面以使用网页 Chat：它接受确切的已发布运行时身份，而不再只接受已退役的 0.9.13。" } },
  ],
};
