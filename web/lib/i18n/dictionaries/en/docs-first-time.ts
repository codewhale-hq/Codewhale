import type { DocsFirstTimeDict } from "../types";

export const docsFirstTime: DocsFirstTimeDict = {
  metaTitle: "Use Codewhale for the first time · Codewhale Docs",
  metaDescription:
    "Open a terminal, install Codewhale, pick the folder it works in, connect a model and finish a first task, with no terminal experience needed.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Use Codewhale for the first time",
  lede:
    "This page takes you from opening a terminal for the first time to finishing a small task with Codewhale. It also explains which folder to start in, because that folder decides what Codewhale can change.",
  sections: [
    {
      id: "what-you-need",
      title: "Check what you need",
      blocks: [
        {
          p: "You need a Mac, a Windows x64 PC or a Linux computer, and an internet connection for the install. To get answers you also need a model: an API key from a hosted provider such as DeepSeek, or a model running on your own computer with Ollama.",
        },
        {
          p: "Codewhale is free and needs no account. A hosted provider bills you directly for what you use, and a model you run yourself needs no key.",
        },
      ],
    },
    {
      id: "open-terminal",
      title: "Open a terminal",
      blocks: [
        {
          p: "A terminal is a window where you type commands instead of clicking. A command runs when you press Return, which is Enter on Windows.",
        },
        {
          rows: [
            ["macOS", "Press Cmd+Space, type Terminal and press Return."],
            ["Windows", "Open the Start menu, type PowerShell and press Enter."],
            ["Linux", "Open your applications menu and search for Terminal. On Ubuntu, Ctrl+Alt+T opens one."],
          ],
        },
        {
          note: "Paste with Cmd+V on a Mac and Ctrl+V on Windows. Most Linux terminals paste with Ctrl+Shift+V.",
        },
      ],
    },
    {
      id: "install",
      title: "Install Codewhale",
      blocks: [
        {
          p: "On a Mac or Linux, paste this command and press Return. It downloads Codewhale, checks the download, and adds Codewhale to your PATH, the list of folders your terminal searches for commands.",
        },
        { code: "curl -fsSL https://codewhale.net/install.sh | sh", lang: "macOS · Linux" },
        { p: "On Windows, paste this command and press Enter:" },
        { code: "winget install --id HunterBown.CodeWhale --exact --source winget", lang: "Windows" },
        {
          p: "When it finishes, open a new terminal window so it picks up the new PATH. Then check that it worked: a version number means Codewhale is installed.",
        },
        { code: "codewhale --version", lang: "Terminal" },
        {
          p: "If the terminal says `command not found`, follow [the PATH steps on the install page](/install#install-path).",
        },
      ],
    },
    {
      id: "choose-folder",
      title: "Choose the folder to work in",
      blocks: [
        {
          p: "Codewhale works in the folder your terminal is in when you start it. That folder is its workspace: Codewhale reads and changes files there, and its file tools cannot reach other folders unless you allow it.",
        },
        {
          p: "On a Mac, an operating-system sandbox also stops its commands from writing outside the workspace, apart from temporary files. Linux gets the same sandbox when bubblewrap is installed. Windows has no such sandbox yet, so Codewhale relies on asking you first.",
        },
        {
          rows: [
            ["A folder for one project", "The best choice for most work. Codewhale sees only that project, and `/undo` can take back what it changed."],
            ["A new, empty folder", "A good start for something new, such as a website or a script. The next section shows how to make one."],
            ["A folder that holds several projects", "Lets one task read and change files across all of them. Pick the smallest folder that holds everything the task needs; in a very large folder Codewhale turns snapshots off and tells you."],
            ["Your home folder, Desktop, Documents or Downloads", "Codewhale runs there, but `/undo` and snapshots are off in these folders themselves because they are too broad to snapshot safely. A folder inside one of them works normally."],
          ],
        },
        {
          note: "Snapshots are Codewhale's own copies of the files it changes. It keeps them in its settings folder, not in your project, and they work without git.",
        },
        {
          p: "To let Codewhale reach one more folder, type `/trust add` and that folder's path in Codewhale. Full Access mode reaches every folder and runs commands without the sandbox, so use it only when you want Codewhale to work across your whole computer.",
        },
        {
          p: "By default, Codewhale makes file edits in a git repository without asking and shows you what changed. In a folder without git, it asks before each edit.",
        },
      ],
    },
    {
      id: "open-in-folder",
      title: "Open a terminal in that folder",
      blocks: [
        {
          rows: [
            ["macOS Finder", "Control-click the folder and choose Services > New Terminal at Folder. If it is missing, turn it on in System Settings > Keyboard > Keyboard Shortcuts > Services > Files and Folders."],
            ["Windows File Explorer", "Right-click the folder and choose Open in Terminal. On Windows 10, hold Shift while you right-click and choose Open PowerShell window here."],
            ["Any terminal", "Type `cd` and a space, drag the folder from Finder or File Explorer into the window, and press Return."],
            ["Without moving", "Start Codewhale with `-C` and the folder's path, for example `codewhale -C ~/Projects/my-site` on a Mac."],
          ],
        },
        { p: "These commands find your way around from inside a terminal. They work on a Mac, on Linux and in PowerShell." },
        {
          codeTerms: true,
          rows: [
            ["pwd", "Shows the folder you are in."],
            ["ls", "Lists what is in it."],
            ["cd Documents", "Moves into the Documents folder inside the current one."],
            ["cd ..", "Moves up one folder."],
            ["mkdir my-site", "Makes a new folder called my-site."],
          ],
        },
        {
          note: "Type the first letters of a folder name and press Tab, and the terminal finishes the name for you.",
        },
      ],
    },
    {
      id: "connect",
      title: "Start Codewhale and connect a model",
      blocks: [
        { code: "codewhale", lang: "Terminal" },
        {
          p: "Codewhale opens a message box right away. When you send your first message, it asks where your model runs: pick a hosted provider and paste its API key, or pick Ollama when it is running on your computer.",
        },
        {
          p: "Keys you enter stay on your computer, in Codewhale's settings folder `~/.codewhale`. Change the provider later with `/provider`.",
        },
        {
          p: "If Codewhale asks whether to work with the instructions and files in this folder, answer yes for a folder you made yourself.",
        },
      ],
    },
    {
      id: "first-task",
      title: "Give it a first task",
      blocks: [
        {
          p: "Type what you want in plain words and press Return. A safe first task changes nothing, for example: `Explain what is in this folder and what each file is for. Do not change anything.`",
        },
        {
          p: "Then try one that makes a file, for example: `Make a file called packing-list.md with a packing list for a weekend trip.`",
        },
        {
          p: "By default, Codewhale asks before it runs a command that is not read-only. Read what the approval says will run before you allow it.",
        },
      ],
    },
    {
      id: "stop-undo-quit",
      title: "Stop, undo and quit",
      blocks: [
        {
          codeTerms: true,
          rows: [
            ["Esc", "Stops the task that is running. Ctrl+C does the same."],
            ["/undo", "Takes back your last request and the file changes it made."],
            ["/diff", "Shows what changed in this folder."],
            ["/receipts", "Lists what Codewhale did: files, commands and approvals."],
            ["/exit", "Quits Codewhale. Ctrl+D also quits when the message box is empty."],
            ["codewhale --continue", "Run it in the same folder to pick up your last conversation there."],
          ],
        },
      ],
    },
    {
      id: "help",
      title: "Get help",
      blocks: [
        {
          p: "Type `/help` in Codewhale to list its commands. [Ask in the Codewhale Discord](https://discord.gg/37gfS3ksug), or read [what to do when something goes wrong](/docs/troubleshooting).",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/guide",
      label: "Start your first task",
      note: "More first prompts and how to check the answer.",
    },
    {
      href: "/docs/auth",
      label: "Connect a provider",
      note: "Save a key, check which key is used, or run a local model.",
    },
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "Decide which commands and edits stop for your approval.",
    },
  ],
  sourceNote:
    "Source documents: docs/GUIDE.md, docs/INSTALL.md, docs/MODES.md, docs/SANDBOX.md, docs/KEYBINDINGS.md · Update docs-map.ts when changing.",
};
