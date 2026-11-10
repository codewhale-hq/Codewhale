/**
 * Installation commands shared by the install page and homepage hero.
 * Code-owned shell, never translated.
 */
export const INSTALL_COMMANDS = {
  shell: "curl -fsSL https://codewhale.net/install.sh | sh",
  npm: "npm install -g codewhale",
  windows: "winget install --id HunterBown.CodeWhale --exact --source winget",
  cargo: "cargo install codewhale-cli --locked",
} as const;

export const INSTALL_COPY = {
  windowsNote: { en: "Windows x64. Winget updates can lag behind GitHub releases; npm is another option.", zh: "适用于 Windows x64。Winget 更新可能晚于 GitHub 发布；也可以通过 npm 安装。" },
  cargoNote: { en: "Builds from source. Requires current stable Rust and build tools. The Cargo package can lag behind GitHub releases.", zh: "从源码构建，需要当前稳定版 Rust 和构建工具。Cargo 包可能晚于 GitHub 发布。" },
  metaTitle: { en: "Install Codewhale, the terminal coding agent", zh: "安装终端编程智能体 Codewhale" },
  metaDescription: { en: "Install the open-source terminal coding agent on macOS, Linux or Windows. Connect your model and run a task.", zh: "在 macOS、Linux 或 Windows 上安装开源终端编程智能体。连接你选择的模型并运行任务。" },
  source: { en: "Read the verified guide on GitHub", zh: "查看 GitHub 上已验证的指南" },
  translationNotice: { en: "Installation receipts retain the version actually tested; latest resolves to the published release.", zh: "本页采用经审阅的简体中文指南。安装回执保留实际测试版本；latest 以已发布版本为准。" },
  heading: { en: "Install Codewhale and try one task", zh: "安装 Codewhale，完成第一个任务" },
  lead: { en: "Choose your platform below and run the command in your terminal. These commands install published packages; package-manager versions can lag behind the latest GitHub release.", zh: "选择下方平台，在终端中运行安装命令。这些命令安装已发布的包；包管理器中的版本可能晚于 GitHub 最新发布。" },
  verify: { en: "Check that the installation worked", zh: "确认安装成功" },
  verifyBody: { en: "Open a new terminal if the installer changed your PATH, then run the version command. It should print a Codewhale version; if the command is missing or fails, follow the troubleshooting guide below.", zh: "如果安装程序修改了 PATH，请打开新终端，再运行版本命令。命令应显示 Codewhale 版本；如果找不到命令或运行失败，请查看下方排查指南。" },
  firstTask: { en: "Connect a model and try your first task", zh: "连接模型，尝试第一个任务" },
  firstTaskBody: { en: "Installation does not connect a model. The quickstart walks through connecting your provider and checking a read-only answer against a real project file.", zh: "安装不会自动连接模型。快速入门会带你连接提供商，并对照实际项目文件检查一次只读回答。" },
  troubleshooting: { en: "Fix an installation problem", zh: "解决安装问题" },
  fullGuide: { en: "Read the complete installation guide", zh: "阅读完整安装指南" },
  configuration: { en: "Settings and configuration", zh: "设置与配置" },
} as const;
