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

export const INSTALL_PAGE_COMMANDS = {
  noModifyPath: "curl -fsSL https://codewhale.net/install.sh | sh -s -- --no-modify-path",
  pathBlock: '# >>> codewhale PATH >>>\nexport PATH="$HOME/.local/bin:$PATH"\n# <<< codewhale PATH <<<',
  pathNow: 'export PATH="$HOME/.local/bin:$PATH"; hash -r',
  pathManual: "echo 'export PATH=\"$HOME/.local/bin:$PATH\"' >> ~/.zshrc",
  version: "codewhale --version",
  telemetryOff: "codewhale config set telemetry false",
  installTelemetryOff: "curl -fsSL https://codewhale.net/install.sh | CODEWHALE_TELEMETRY=0 sh",
  update: "codewhale update",
  uninstall: "codewhale uninstall --yes",
  removeFiles: "rm -f ~/.local/bin/codewhale ~/.local/bin/codew ~/.local/bin/.codewhale-install.sha256",
  removeData: "rm -rf ~/.codewhale",
  windowsUpgrade: "winget upgrade HunterBown.CodeWhale",
  npmUpdate: "npm install -g codewhale@latest",
  npmRemove: "npm uninstall -g codewhale",
  npmPrefix: 'npm config set prefix "$HOME/.npm-global"\necho \'export PATH="$HOME/.npm-global/bin:$PATH"\' >> ~/.bashrc\nexport PATH="$HOME/.npm-global/bin:$PATH"',
  cargoUpdate: "cargo install codewhale-cli --locked --force",
  cargoRemove: "cargo uninstall codewhale-cli",
  brewInstall: "brew install Hmbown/deepseek-tui/codewhale",
  brewUpdate: "brew upgrade codewhale",
  brewRemove: "brew uninstall codewhale && brew untap Hmbown/deepseek-tui",
  cnbVerify: "sha256sum -c codewhale-artifacts-sha256.txt --ignore-missing",
} as const;

export const PATH_MARKER_START = "# >>> codewhale PATH >>>";
export const PATH_MARKER_END = "# <<< codewhale PATH <<<";

export const INSTALL_COPY = {
  windowsNote: { en: "Windows x64. Winget updates can lag behind GitHub releases; npm is another option.", zh: "适用于 Windows x64。Winget 更新可能晚于 GitHub 发布；也可以通过 npm 安装。" },
  cargoNote: { en: "Builds from source. Requires current stable Rust and build tools. The Cargo package can lag behind GitHub releases.", zh: "从源码构建，需要当前稳定版 Rust 和构建工具。Cargo 包可能晚于 GitHub 发布。" },
  metaTitle: { en: "Install Codewhale", zh: "安装 Codewhale" },
  metaDescription: {
    en: "Install Codewhale, formerly DeepSeek TUI, on macOS, Linux or Windows. You need no account.",
    zh: "在 macOS、Linux 或 Windows 上安装 Codewhale（原 DeepSeek TUI）。你无需账号。",
  },

  scriptLink: { en: "View install script", zh: "查看安装脚本" },
  releaseLine: { en: "This installs the latest published release, v{version}.", zh: "这会安装最新已发布版本 v{version}。" },

  newTitle: { en: "New to the terminal?", zh: "第一次使用终端？" },
  newLead: {
    en: "You install Codewhale by pasting one command into a terminal. These steps show how to open one and how to check that it worked.",
    zh: "你在终端中粘贴一条命令来安装 Codewhale。下面的步骤说明如何打开终端，以及如何确认安装成功。",
  },
  newMacTitle: { en: "macOS", zh: "macOS" },
  newMac1: { en: "Press Cmd+Space, type Terminal and press Return.", zh: "按 Cmd+空格键，输入 Terminal，然后按回车键。" },
  newMac2: { en: "Paste this command with Cmd+V and press Return:", zh: "用 Cmd+V 粘贴下面这条命令，然后按回车键：" },
  newMac3: {
    en: "When it finishes, open a new window with Cmd+N, type `codewhale --version` and press Return. A version number means it worked.",
    zh: "安装完成后，按 Cmd+N 打开一个新窗口，输入 `codewhale --version` 并按回车键。显示版本号就说明安装成功了。",
  },
  newWinTitle: { en: "Windows", zh: "Windows" },
  newWin1: { en: "Open the Start menu, type PowerShell and press Enter.", zh: "打开“开始”菜单，输入 PowerShell，然后按回车键。" },
  newWin2: { en: "Paste this command with Ctrl+V and press Enter:", zh: "用 Ctrl+V 粘贴下面这条命令，然后按回车键：" },
  newWin3: {
    en: "When it finishes, close the window, open PowerShell again, type `codewhale --version` and press Enter. A version number means it worked.",
    zh: "安装完成后，关闭窗口，重新打开 PowerShell，输入 `codewhale --version` 并按回车键。显示版本号就说明安装成功了。",
  },
  newNext: {
    en: "Read the first-time guide: which folder to work in, your first task, and how to undo",
    zh: "阅读首次使用指南：在哪个文件夹工作、第一个任务，以及如何撤回",
  },

  pathTitle: { en: "What the installer does to your PATH", zh: "安装程序对 PATH 做了什么" },
  pathPlaces: {
    en: "The installer puts `codewhale` and `codew` in `~/.local/bin`. When that folder is not on your PATH, it adds three marked lines to the startup file for your shell:",
    zh: "安装程序会把 `codewhale` 和 `codew` 放进 `~/.local/bin`。当该文件夹不在你的 PATH 中时，它会向你所用 shell 的启动文件添加三行带标记的内容：",
  },
  pathZsh: { en: "zsh: `~/.zshrc`, or `.zshrc` in `$ZDOTDIR` when you set it.", zh: "zsh：`~/.zshrc`；如果设置了 `$ZDOTDIR`，则是其中的 `.zshrc`。" },
  pathBashLinux: { en: "bash on Linux: `~/.bashrc`.", zh: "Linux 上的 bash：`~/.bashrc`。" },
  pathBashMac: {
    en: "bash on macOS: `~/.bash_profile`, or `~/.bash_login` or `~/.profile` when one of those exists and `~/.bash_profile` does not.",
    zh: "macOS 上的 bash：`~/.bash_profile`；如果它不存在而 `~/.bash_login` 或 `~/.profile` 已存在，则使用后者。",
  },
  pathOther: { en: "sh, dash, ksh and other POSIX shells: `~/.profile`.", zh: "sh、dash、ksh 等其他 POSIX shell：`~/.profile`。" },
  pathFish: { en: "fish: nothing is edited. The installer prints `fish_add_path` for you to run.", zh: "fish：不会修改任何文件，安装程序会打印 `fish_add_path` 供你自行运行。" },
  pathBlockIntro: { en: "The three lines are:", zh: "这三行是：" },
  pathBlockAfter: {
    en: "The installer prints them and a command that removes them, and changes nothing else in the file. It does not edit your profile when the folder is already on your PATH. Shells that are already open do not reread the profile, so run this line in the shell you installed from, or open a new terminal:",
    zh: "安装程序会打印这三行，以及一条用于删除它们的命令，并且不会改动文件中的其他内容。如果该文件夹已经在你的 PATH 中，它不会修改你的启动文件。已经打开的 shell 不会重新读取启动文件，因此请在你运行安装命令的 shell 中执行下面这一行，或打开一个新终端：",
  },
  pathOptOut: {
    en: "To leave your startup file alone, run the installer with `--no-modify-path`, or set `CODEWHALE_NO_MODIFY_PATH=1`:",
    zh: "如果不想让安装程序修改启动文件，请加上 `--no-modify-path` 运行，或设置 `CODEWHALE_NO_MODIFY_PATH=1`：",
  },
  pathManualIntro: {
    en: "Then add this line to your shell's startup file yourself. Use `~/.bashrc` for bash. For fish, run `fish_add_path ~/.local/bin`.",
    zh: "然后自行把下面这一行加入你所用 shell 的启动文件。bash 请使用 `~/.bashrc`；fish 请运行 `fish_add_path ~/.local/bin`。",
  },

  verifyTitle: { en: "Check that it worked", zh: "确认安装成功" },
  verifyBody: {
    en: "Run this in a shell that has the PATH line. It prints `codewhale {version}` followed by a short commit id in parentheses.",
    zh: "请在已带有 PATH 设置的 shell 中运行。它会打印 `codewhale {version}`，后面括号里是一个简短的提交编号。",
  },
  verifyBodyUnknown: {
    en: "Run this in a shell that has the PATH line. It prints `codewhale` and its version.",
    zh: "请在已带有 PATH 设置的 shell 中运行。它会打印 `codewhale` 及其版本号。",
  },

  firstTitle: { en: "Your first task", zh: "你的第一个任务" },
  firstStep1: { en: "Run `codewhale` in a git repository.", zh: "在 git 仓库中运行 `codewhale`。" },
  firstStep2: {
    en: "Type `/provider` before your first message and choose a provider, or paste a Custom endpoint.",
    zh: "在发送第一条消息之前输入 `/provider`，选择一个提供商，或粘贴自定义端点。",
  },
  firstStep3: {
    en: "Give it a small task. It asks before it runs a command. Then type `/receipts` to read what it did, and `/cost` to see what it cost or why the cost is unknown.",
    zh: "给它一个小任务。它会在运行命令之前询问你。然后输入 `/receipts` 查看它做了什么，输入 `/cost` 查看费用，或查看费用未知的原因。",
  },
  quickstart: { en: "Read the quickstart", zh: "阅读快速入门" },

  usageTitle: { en: "Usage counting", zh: "使用统计" },
  usageBody: {
    en: "Usage counting is on by default. Run `codewhale config set telemetry false` to turn it off.",
    zh: "使用统计默认开启。运行 `codewhale config set telemetry false` 可将其关闭。",
  },
  usageInstaller: {
    en: "The installer runs `codewhale` when it finishes. To turn counting off for that run, set `CODEWHALE_TELEMETRY=0` for the installer:",
    zh: "安装程序在结束时会运行一次 `codewhale`。要在这次运行中关闭统计，请为安装程序设置 `CODEWHALE_TELEMETRY=0`：",
  },
  usageLink: { en: "What is counted", zh: "统计的内容" },

  updateTitle: { en: "Update", zh: "更新" },
  updateBody: {
    en: "Run `codewhale update`. If you installed with npm, Cargo or Homebrew, update with that tool instead, as shown below.",
    zh: "运行 `codewhale update`。如果你是通过 npm、Cargo 或 Homebrew 安装的，请改用对应的工具更新，见下文。",
  },

  removeTitle: { en: "Remove it", zh: "卸载" },
  removeNow: {
    en: "Run `codewhale uninstall --yes`. Your data in `~/.codewhale` is kept.",
    zh: "运行 `codewhale uninstall --yes`。`~/.codewhale` 中的数据会保留。",
  },
  removeFilesNow: { en: "To remove an older release by hand, delete its files:", zh: "要手动卸载较早的版本，请删除它的文件：" },
  removeFilesOnly: { en: "Removal for v{version}: delete the program files.", zh: "v{version} 的卸载方法：删除程序文件。" },
  removeFilesOnlyUnknown: { en: "Removal for the released version: delete the program files.", zh: "已发布版本的卸载方法：删除程序文件。" },
  removePath: {
    en: "Then delete the three lines from `# >>> codewhale PATH >>>` to `# <<< codewhale PATH <<<`, inclusive, from the startup file the installer named.",
    zh: "然后从安装程序指出的启动文件中，删除从 `# >>> codewhale PATH >>>` 到 `# <<< codewhale PATH <<<` 的三行（含这两行标记）。",
  },
  removeData: {
    en: "`~/.codewhale` holds your settings, sessions and any keys you saved. Delete it only if you want them gone:",
    zh: "`~/.codewhale` 保存着你的设置、会话和你保存过的密钥。只有在你想一并删除它们时才删除：",
  },
  removeGuide: {
    en: "The v{version} guide also lists `~/.deepseek/snapshots` and each repository's `.codewhale` folder as places Codewhale writes.",
    zh: "v{version} 的指南还列出了 `~/.deepseek/snapshots` 以及每个仓库中的 `.codewhale` 文件夹，它们也是 Codewhale 写入数据的位置。",
  },

  troubleTitle: { en: "If something goes wrong", zh: "遇到问题时" },
  troubleNotFound: {
    en: "If your shell says `codewhale: command not found`, it has not picked up the PATH line yet. Run the line shown above in that shell, or open a new terminal.",
    zh: "如果 shell 提示 `codewhale: command not found`，说明它还没有读取到 PATH 设置。请在该 shell 中运行上面的那一行，或打开一个新终端。",
  },
  troubleQueued: {
    en: "If messages say they were saved for later after you connect, quit with Ctrl+D and run `codewhale` again.",
    zh: "如果在你连接提供商之后，消息仍显示为稍后保存，请按 Ctrl+D 退出，然后重新运行 `codewhale`。",
  },
  troubleLegacy: {
    en: "A Custom (OpenAI-compatible) row in `/provider` may show \"legacy ! provider admission failed\" although the endpoint works.",
    zh: "`/provider` 中的“Custom (OpenAI-compatible)”一行可能显示“legacy ! provider admission failed”，但该端点实际可以正常使用。",
  },
  troubleMore: { en: "More fixes", zh: "更多解决办法" },

  channelsTitle: { en: "Other ways to install", zh: "其他安装方式" },
  channelsLead: {
    en: "Each channel updates and removes with its own tool. The steps here are the ones in the v{version} guide.",
    zh: "每种安装方式都用各自的工具更新和卸载。这里的步骤与 v{version} 的指南一致。",
  },
  channelsLeadUnknown: {
    en: "Each channel updates and removes with its own tool.",
    zh: "每种安装方式都用各自的工具更新和卸载。",
  },

  windowsTitle: { en: "Windows", zh: "Windows" },
  windowsVerify: { en: "Open a new terminal and run `codewhale --version`.", zh: "打开一个新终端并运行 `codewhale --version`。" },
  windowsSandbox: {
    en: "Windows has no operating-system sandbox yet. In Ask, Codewhale asks before shell commands that are not read-only.",
    zh: "Windows 目前没有操作系统级沙箱。在 Ask 下，Codewhale 会在运行非只读的 shell 命令之前询问你。",
  },
  windowsPackage: {
    en: "The winget package is for Windows x64 and can lag behind GitHub releases. On Windows ARM64, use npm under native ARM64 Node.js.",
    zh: "winget 包适用于 Windows x64，可能晚于 GitHub 发布。在 Windows ARM64 上，请在原生 ARM64 的 Node.js 下使用 npm。",
  },
  windowsUpdate: {
    en: "Update with `winget upgrade HunterBown.CodeWhale`, or with npm if you installed with npm. Remove it with winget or with npm, whichever installed it.",
    zh: "使用 `winget upgrade HunterBown.CodeWhale` 更新；如果是通过 npm 安装的，则用 npm 更新。卸载时使用当初安装它的 winget 或 npm。",
  },

  npmTitle: { en: "npm", zh: "npm" },
  npmBody: {
    en: "npm needs Node.js 18 or newer and a global folder you can write to, and that folder's `bin` must be on your PATH. On Windows, Node stays the launcher for the whole session, so do not stop it by process name.",
    zh: "npm 需要 Node.js 18 或更高版本，以及一个你有写入权限的全局文件夹，并且该文件夹的 `bin` 必须在 PATH 中。在 Windows 上，Node 会在整个会话期间作为启动器，因此不要按进程名终止它。",
  },
  npmEacces: {
    en: "If npm stops with `EACCES`, do not use sudo. Point npm at a folder you own:",
    zh: "如果 npm 因 `EACCES` 而停止，不要使用 sudo。请让 npm 使用你自己拥有的文件夹：",
  },
  npmUpdate: {
    en: "Update with `npm install -g codewhale@latest`. Remove the program with `npm uninstall -g codewhale`; it leaves `~/.codewhale`.",
    zh: "使用 `npm install -g codewhale@latest` 更新。使用 `npm uninstall -g codewhale` 卸载程序；它会保留 `~/.codewhale`。",
  },

  cargoTitle: { en: "Cargo", zh: "Cargo" },
  cargoBody: {
    en: "Cargo builds from source. It needs current stable Rust, and on Linux `libdbus-1-dev` and `pkg-config`. It installs `codewhale` only, with no `codew`. Install `codewhale-cli`: the Cargo package named `codewhale` is an older package.",
    zh: "Cargo 从源码构建，需要当前稳定版 Rust；在 Linux 上还需要 `libdbus-1-dev` 和 `pkg-config`。它只安装 `codewhale`，不含 `codew`。请安装 `codewhale-cli`：名为 `codewhale` 的 Cargo 包是一个较早的旧包。",
  },
  cargoUpdate: { en: "Update with the command below. Remove it with `cargo uninstall codewhale-cli`.", zh: "使用下面的命令更新。使用 `cargo uninstall codewhale-cli` 卸载。" },

  brewTitle: { en: "Homebrew on Linux", zh: "Linux 上的 Homebrew" },
  brewBody: {
    en: "The full name taps and trusts in one step. The two-step form fails on Homebrew 7.",
    zh: "使用完整名称可以一步完成 tap 和信任。两步写法在 Homebrew 7 上会失败。",
  },
  brewUpdate: { en: "Update with `brew upgrade codewhale`. Remove it with:", zh: "使用 `brew upgrade codewhale` 更新。使用下面的命令卸载：" },

  mirrorsTitle: { en: "Mirrors for mainland China", zh: "中国大陆镜像" },
  mirrorsNpm: {
    en: "npmmirror serves the npm package. The program it installs still downloads from a release asset source: GitHub, or the CNB mirror, which the npm package tries on Linux x64.",
    zh: "npmmirror 提供 npm 包。它安装的程序仍然要从发布资源来源下载：GitHub，或 CNB 镜像（npm 包在 Linux x64 上会自动尝试 CNB）。",
  },
  mirrorsCnb: {
    en: "The CNB mirror carries the source of each release tag and, for tags it has built, Linux x64 binaries with a checksum manifest. A mirrored source tag is not a binary release, so confirm the version you want has binaries and a manifest, then verify the download:",
    zh: "CNB 镜像保存每个发布标签的源码；对于它已构建的标签，还提供 Linux x64 二进制文件及校验清单。镜像的源码标签不等于二进制发布，请先确认你要的版本有二进制文件和校验清单，再校验下载文件：",
  },
  mirrorsGuide: { en: "Read the mirror guide", zh: "阅读镜像指南" },

  fullGuide: { en: "Read the full installation guide on GitHub", zh: "在 GitHub 上阅读完整安装指南" },
} as const;
