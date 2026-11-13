import type { DocsFirstTimeDict } from "../types";

export const docsFirstTime: DocsFirstTimeDict = {
  metaTitle: "第一次使用 Codewhale · Codewhale 文档",
  metaDescription:
    "打开终端、安装 Codewhale、选择它工作的文件夹、连接模型并完成第一个任务，不需要任何终端经验。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "第一次使用 Codewhale",
  lede:
    "这一页带你从第一次打开终端开始，一直到用 Codewhale 完成一个小任务。它还会说明应该从哪个文件夹启动，因为这个文件夹决定了 Codewhale 能修改什么。",
  sections: [
    {
      id: "what-you-need",
      title: "确认你需要准备什么",
      blocks: [
        {
          p: "你需要一台 Mac、一台 Windows x64 电脑或一台 Linux 电脑，安装时还需要联网。要得到回答，你还需要一个模型：来自 DeepSeek 等托管提供商的 API 密钥，或者用 Ollama 在你自己电脑上运行的模型。",
        },
        {
          p: "Codewhale 免费，也不需要账号。托管提供商会按你的用量直接向你收费；你自己运行的模型不需要密钥。",
        },
      ],
    },
    {
      id: "open-terminal",
      title: "打开终端",
      blocks: [
        {
          p: "终端是一个用输入命令代替点击的窗口。按下回车键后，命令就会运行。",
        },
        {
          rows: [
            ["macOS", "按 Cmd+空格键，输入 Terminal，然后按回车键。"],
            ["Windows", "打开“开始”菜单，输入 PowerShell，然后按回车键。"],
            ["Linux", "打开应用程序菜单并搜索“终端”。在 Ubuntu 上，按 Ctrl+Alt+T 也能打开。"],
          ],
        },
        {
          note: "在 Mac 上用 Cmd+V 粘贴，在 Windows 上用 Ctrl+V 粘贴。大多数 Linux 终端用 Ctrl+Shift+V 粘贴。",
        },
      ],
    },
    {
      id: "install",
      title: "安装 Codewhale",
      blocks: [
        {
          p: "在 Mac 或 Linux 上，粘贴下面这条命令并按回车键。它会下载 Codewhale、校验下载内容，并把 Codewhale 加入 PATH，也就是终端查找命令时会搜索的文件夹列表。",
        },
        { code: "curl -fsSL https://codewhale.net/install.sh | sh", lang: "macOS · Linux" },
        { p: "在 Windows 上，粘贴下面这条命令并按回车键：" },
        { code: "winget install --id HunterBown.CodeWhale --exact --source winget", lang: "Windows" },
        {
          p: "安装完成后，打开一个新的终端窗口，让它读取新的 PATH。然后确认安装成功：显示版本号就说明 Codewhale 已经安装好了。",
        },
        { code: "codewhale --version", lang: "终端" },
        {
          p: "如果终端提示 `command not found`，请按照[安装页面上的 PATH 步骤](/install#install-path)操作。",
        },
      ],
    },
    {
      id: "choose-folder",
      title: "选择工作的文件夹",
      blocks: [
        {
          p: "Codewhale 在你启动它时终端所在的文件夹里工作。这个文件夹就是它的工作区：Codewhale 在这里读取和修改文件，除非你允许，它的文件工具无法访问其他文件夹。",
        },
        {
          p: "在 Mac 上，操作系统沙箱还会阻止它的命令写入工作区以外的位置（临时文件除外）。在 Linux 上，安装 bubblewrap 后也有同样的沙箱。Windows 目前还没有这种沙箱，因此 Codewhale 依靠先征求你的同意。",
        },
        {
          rows: [
            ["只放一个项目的文件夹", "大多数情况下的最佳选择。Codewhale 只看到这个项目，`/undo` 可以撤回它做的修改。"],
            ["一个新的空文件夹", "适合开始做新东西，比如一个网站或一个脚本。下一节会说明如何创建。"],
            ["包含多个项目的文件夹", "让一个任务可以读取和修改所有这些项目里的文件。请选择能装下任务所需全部内容的最小文件夹；文件夹非常大时，Codewhale 会关闭快照并告诉你。"],
            ["你的主文件夹、桌面、文稿或下载", "Codewhale 可以在这里运行，但在这些文件夹本身中 `/undo` 和快照是关闭的，因为它们范围太大，无法安全地做快照。它们里面的子文件夹可以正常使用。"],
          ],
        },
        {
          note: "快照是 Codewhale 为它修改的文件保存的副本。它们保存在 Codewhale 的设置文件夹里，而不是你的项目里，并且不需要 git。",
        },
        {
          p: "要让 Codewhale 多访问一个文件夹，请在 Codewhale 中输入 `/trust add` 和该文件夹的路径。Full Access 模式可以访问所有文件夹，并且在没有沙箱的情况下运行命令，所以只有在你希望 Codewhale 在整台电脑范围内工作时才使用它。",
        },
        {
          p: "默认情况下，在 git 仓库中 Codewhale 会直接修改文件，并向你展示改了什么。在没有 git 的文件夹中，它会在每次修改前询问你。",
        },
      ],
    },
    {
      id: "open-in-folder",
      title: "在那个文件夹中打开终端",
      blocks: [
        {
          rows: [
            ["macOS 访达", "按住 Control 点按该文件夹，选择“服务”>“新建位于文件夹位置的终端窗口”。如果没有这一项，请在“系统设置”>“键盘”>“键盘快捷键”>“服务”>“文件和文件夹”中打开它。"],
            ["Windows 文件资源管理器", "右键点击该文件夹，选择“在终端中打开”。在 Windows 10 上，按住 Shift 再右键点击，选择“在此处打开 PowerShell 窗口”。"],
            ["任意终端", "输入 `cd` 和一个空格，把文件夹从访达或文件资源管理器拖进窗口，然后按回车键。"],
            ["不切换文件夹", "启动 Codewhale 时加上 `-C` 和文件夹路径，例如在 Mac 上输入 `codewhale -C ~/Projects/my-site`。"],
          ],
        },
        { p: "下面这些命令可以在终端里切换和查看文件夹。它们在 Mac、Linux 和 PowerShell 中都能用。" },
        {
          codeTerms: true,
          rows: [
            ["pwd", "显示你当前所在的文件夹。"],
            ["ls", "列出其中的内容。"],
            ["cd Documents", "进入当前文件夹里的 Documents 文件夹。"],
            ["cd ..", "回到上一级文件夹。"],
            ["mkdir my-site", "新建一个名为 my-site 的文件夹。"],
          ],
        },
        {
          note: "输入文件夹名的前几个字母后按 Tab 键，终端会帮你补全名字。",
        },
      ],
    },
    {
      id: "connect",
      title: "启动 Codewhale 并连接模型",
      blocks: [
        { code: "codewhale", lang: "终端" },
        {
          p: "Codewhale 会立即打开一个输入框。你发送第一条消息时，它会询问模型在哪里运行：选择一个托管提供商并粘贴它的 API 密钥，或者在你的电脑上运行着 Ollama 时选择 Ollama。",
        },
        {
          p: "你输入的密钥保存在你自己的电脑上，位于 Codewhale 的设置文件夹 `~/.codewhale` 中。之后可以用 `/provider` 更换提供商。",
        },
        {
          p: "如果 Codewhale 询问是否使用这个文件夹中的说明和文件，对于你自己创建的文件夹，回答“是”即可。",
        },
      ],
    },
    {
      id: "first-task",
      title: "交给它第一个任务",
      blocks: [
        {
          p: "用平常的话写下你想做的事，然后按回车键。安全的第一个任务不修改任何东西，例如：`说明这个文件夹里有什么，以及每个文件的用途。不要修改任何内容。`",
        },
        {
          p: "然后试一个会创建文件的任务，例如：`创建一个名为 packing-list.md 的文件，写一份周末出游的行李清单。`",
        },
        {
          p: "默认情况下，Codewhale 在运行非只读命令前会先询问你。在允许之前，请先看清审批里写明要运行什么。",
        },
      ],
    },
    {
      id: "stop-undo-quit",
      title: "停止、撤回和退出",
      blocks: [
        {
          codeTerms: true,
          rows: [
            ["Esc", "停止正在运行的任务。Ctrl+C 的作用相同。"],
            ["/undo", "撤回你的上一个请求及其造成的文件修改。"],
            ["/diff", "显示这个文件夹中改了什么。"],
            ["/receipts", "列出 Codewhale 做过的事：文件、命令和审批。"],
            ["/exit", "退出 Codewhale。输入框为空时，按 Ctrl+D 也会退出。"],
            ["codewhale --continue", "在同一个文件夹中运行它，可以继续你在那里的上一次对话。"],
          ],
        },
      ],
    },
    {
      id: "help",
      title: "获取帮助",
      blocks: [
        {
          p: "在 Codewhale 中输入 `/help` 可以列出它的命令。你也可以[在 Codewhale Discord 中提问](https://discord.gg/37gfS3ksug)，或阅读[出现问题时怎么办](/docs/troubleshooting)。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/guide",
      label: "开始第一个任务",
      note: "更多第一个提示词的示例，以及如何检查回答。",
    },
    {
      href: "/docs/auth",
      label: "连接模型提供商",
      note: "保存密钥、查看当前使用的密钥，或运行本地模型。",
    },
    {
      href: "/docs/modes",
      label: "设置模式与审批",
      note: "决定哪些命令和修改需要你批准。",
    },
  ],
  sourceNote:
    "来源文档：docs/GUIDE.md、docs/INSTALL.md、docs/MODES.md、docs/SANDBOX.md、docs/KEYBINDINGS.md · 修改时请同步更新 docs-map.ts。",
};
