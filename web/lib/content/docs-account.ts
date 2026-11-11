import type { DocsPageDict } from "../i18n/dictionaries/types";

const EN: DocsPageDict = {
  metaTitle: "Sign in to Codewhale · Codewhale Docs",
  metaDescription:
    "How to sign in to Codewhale from the web, the desktop app and the terminal, how to sign out, and where to export or delete your data. The terminal and codewhale web need no account.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Sign in to Codewhale",
  lede:
    "A Codewhale account is optional, and anyone can create one by signing in. This page shows how to sign in from the web, the desktop app or the terminal, and how to sign out again. Cloud computers are invite-only.",
  sections: [
    {
      id: "no-account",
      title: "What needs no account",
      blocks: [
        {
          p: "You can use Codewhale today without signing in anywhere. These surfaces run on your machine with your own provider key or a local model:",
        },
        {
          list: [
            "The terminal: run `codewhale` in a project folder.",
            "The local browser client: run `codewhale web` and use it in a browser tab on the same machine. See [Open the browser client](/docs/web).",
            "Local work in the desktop app, which is a development build with no public download yet.",
          ],
        },
        {
          p: "A provider key and a Codewhale account are separate. To connect a model, follow [Connect a provider](/docs/auth).",
        },
      ],
    },
    {
      id: "web",
      title: "Sign in on the web",
      blocks: [
        {
          p: "Open `app.codewhale.net/login` and choose how to continue: Continue with GitHub, Continue with Google, or Continue with email. For email, Codewhale sends a code or a link to your address, depending on how sign-in is set up; follow it to finish. The page needs JavaScript.",
        },
        {
          note: "The hosted browser app at app.codewhale.net is not open yet. Its sign-in page is live, and it is the page where you approve a sign-in that you started from the desktop app or the terminal.",
        },
      ],
    },
    {
      id: "desktop",
      title: "Sign in from the desktop app",
      blocks: [
        {
          note: "The desktop app is a development build. There is no public download yet, and these steps describe the build as it stands today.",
        },
        {
          steps: [
            "In the app, press Sign in. It appears in Settings, Account, and beside features that need an account.",
            "Your browser opens the Codewhale approval page.",
            "Read the page titled \"Sign in to the Codewhale app?\". It lists the app, the device and the account.",
            "Continue only if you started this from the app a moment ago, then press \"Sign in to the app\". Press Cancel if you did not.",
            "The page says you are signed in. Return to the app.",
          ],
        },
        {
          p: "The approval page expires after 10 minutes. The app itself stops waiting after about 2 minutes and says \"That took too long. Try again.\"; either way you start again from the app. Your folders, conversations and model keys stay on your computer either way.",
        },
      ],
    },
    {
      id: "terminal",
      title: "Sign in from the terminal",
      blocks: [
        { p: "Run the login command in any terminal:" },
        { code: "codewhale login", lang: "Terminal" },
        {
          steps: [
            "The terminal prints a one-time code in the form `XXXX-XXXX-XXXX` and a link, then opens the link in your browser. Add `--no-open` to only print them.",
            "On the approval page, check that the code matches the one in your terminal, then approve.",
            "The terminal prints \"Signed in to Codewhale.\"",
          ],
        },
        {
          p: "The command waits for 10 minutes by default; `--timeout-seconds` changes that. Two more commands manage the session for the profile you are using:",
        },
        {
          rows: [
            ["codewhale account status", "Shows which account this profile is signed in to, or says that it is not signed in."],
            ["codewhale account logout", "Removes this profile's account session."],
          ],
          codeTerms: true,
        },
        {
          p: "`codewhale login` is also available as `codewhale account login`. It never takes a provider key; those go through `codewhale auth set`.",
        },
      ],
    },
    {
      id: "headless",
      title: "Use a machine with no browser",
      blocks: [
        {
          p: "On a server or in CI you cannot approve a sign-in in a browser. Use an account API key instead: set `CODEWHALE_API_KEY` to a key that starts with `cwc_key_`. You create the key from a signed-in terminal with `codewhale account api-keys create --name <name>`.",
        },
        {
          p: "While `CODEWHALE_API_KEY` is set, commands that act for your whole account, such as cloud projects and agents, ask you to unset it and run `codewhale login`.",
        },
      ],
    },
    {
      id: "phone",
      title: "Sign in on a phone",
      blocks: [
        {
          p: "The phone apps are not public yet, so there is nothing to sign in to on a phone today.",
        },
      ],
    },
    {
      id: "sign-out",
      title: "Sign out",
      blocks: [
        { p: "Each surface ends only its own session, so signing out in one place does not sign you out of the others." },
        {
          rows: [
            ["Desktop app", "Press Sign out in Settings, Account, or press your name at the bottom of the sidebar and choose Sign out."],
            ["Terminal", "Run `codewhale account logout`."],
            ["Web", "Press Use another account on the signed-in page."],
          ],
        },
        {
          note: "`codewhale logout` is different: it removes every saved provider key and login, and asks you to confirm first. Use `codewhale account logout` to end only the account session.",
        },
      ],
    },
    {
      id: "data",
      title: "Export your data or delete your account",
      blocks: [
        {
          p: "Today you do this in one of two places. In the desktop app, open Settings, Account, Data & privacy. Press Export my data to save a copy, or Schedule account deletion to start deletion. After 30 days Codewhale can permanently delete the account, and you can cancel before deletion starts. Files on your computer stay there.",
        },
        {
          p: "From any surface, you can also email help@codewhale.net and ask for an export or a deletion.",
        },
      ],
    },
    {
      id: "guide",
      title: "The full app guide",
      blocks: [
        {
          p: "The step-by-step guide to the desktop app, including everything it shows when you sign in, is at [docs.codewhale.net](https://docs.codewhale.net).",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/auth",
      label: "Connect a provider",
      note: "Give Codewhale a model with a provider key or a local model.",
    },
    {
      href: "/docs/web",
      label: "Open the browser client",
      note: "Use Codewhale in a browser tab on your own machine, with no account.",
    },
    {
      href: "/docs/trust",
      label: "See what leaves your machine",
      note: "What stays local, what a provider receives, and how to turn usage counting off.",
    },
  ],
  sourceNote:
    "Source: crates/cli (login, account status, account logout, logout) at the published release tag, the desktop app's sign-in flow and Settings, Account · Update docs-map.ts when changing.",
  sourceGroups: [
    {
      claim: { feature: "account-login", describesCurrentRow: true },
      sections: [
        {
          id: "source-account-login",
          title: "Sign in and out inside the terminal app",
          blocks: [
            {
              p: "`/login` opens account sign-in inside the terminal app with a browser link and code, and `/login status` shows the account. Signing in keeps your selected model and unsent message. An account connection is separate from usable model access.",
            },
            {
              p: "`/logout`, `codewhale logout` and `codewhale account logout` end only the account session and keep local model connections and conversations. `codewhale logout --all` removes every saved provider credential as well, and asks you to confirm first.",
            },
          ],
        },
      ],
    },
  ],
};

const ZH: DocsPageDict = {
  metaTitle: "登录 Codewhale · Codewhale 文档",
  metaDescription:
    "如何在网页、桌面应用和终端登录 Codewhale，如何退出登录，以及在哪里导出或删除数据。终端和 codewhale web 无需账户。",
  bodyClassName: "text-ink-soft leading-[1.9] tracking-wide",
  title: "登录 Codewhale",
  lede:
    "Codewhale 账户是可选的，任何人登录即可创建账户。本页说明如何在网页、桌面应用或终端登录，以及如何退出登录。云端计算机仅限受邀使用。",
  sections: [
    {
      id: "no-account",
      title: "哪些用法不需要账户",
      blocks: [
        {
          p: "现在不登录任何账户也可以使用 Codewhale。下面这些入口都在你的机器上运行，使用你自己的提供商密钥或本地模型：",
        },
        {
          list: [
            "终端：在项目文件夹里运行 `codewhale`。",
            "本地浏览器客户端：运行 `codewhale web`，在同一台机器的浏览器标签页中使用。详见[打开浏览器客户端](/docs/web)。",
            "桌面应用中的本地工作。桌面应用目前是开发版本，还没有公开下载。",
          ],
        },
        {
          p: "提供商密钥和 Codewhale 账户是两回事。要连接模型，请看[连接模型提供商](/docs/auth)。",
        },
      ],
    },
    {
      id: "web",
      title: "在网页登录",
      blocks: [
        {
          p: "打开 `app.codewhale.net/login`，选择登录方式：使用 GitHub 继续、使用 Google 继续，或使用邮箱继续。选择邮箱时，Codewhale 会根据登录的设置向你的地址发送验证码或链接，按提示完成即可。该页面需要 JavaScript。",
        },
        {
          note: "app.codewhale.net 上的托管浏览器应用尚未开放。它的登录页是可用的，你在桌面应用或终端发起登录后，就是在这个页面批准。",
        },
      ],
    },
    {
      id: "desktop",
      title: "从桌面应用登录",
      blocks: [
        {
          note: "桌面应用是开发版本，目前没有公开下载。下面的步骤描述的是它今天的状态。",
        },
        {
          steps: [
            "在应用中点击登录。它出现在设置的账户页，也出现在需要账户的功能旁边。",
            "浏览器会打开 Codewhale 的批准页面。",
            "阅读标题为“登录 Codewhale 应用？”的页面。它会列出应用、设备和账户。",
            "只有在你刚刚从应用里发起了登录时才继续，然后点击“登录应用”。如果不是你发起的，请点击取消。",
            "页面提示已登录后，回到应用即可。",
          ],
        },
        {
          p: "批准页面在 10 分钟后失效，而应用本身大约等待 2 分钟后就会提示登录超时。无论哪种情况，你都需要从应用重新开始。无论成功与否，你的文件夹、对话和模型密钥都保留在你的电脑上。",
        },
      ],
    },
    {
      id: "terminal",
      title: "从终端登录",
      blocks: [
        { p: "在任意终端运行登录命令：" },
        { code: "codewhale login", lang: "终端" },
        {
          steps: [
            "终端会打印形如 `XXXX-XXXX-XXXX` 的一次性验证码和一个链接，并在浏览器中打开该链接。加上 `--no-open` 则只打印、不打开。",
            "在批准页面上，确认页面显示的验证码与终端中的一致，然后批准。",
            "终端打印“Signed in to Codewhale.”",
          ],
        },
        {
          p: "该命令默认等待 10 分钟，可用 `--timeout-seconds` 调整。另有两个命令用来管理你当前配置档的会话：",
        },
        {
          rows: [
            ["codewhale account status", "显示这个配置档登录的是哪个账户，或说明尚未登录。"],
            ["codewhale account logout", "移除这个配置档的账户会话。"],
          ],
          codeTerms: true,
        },
        {
          p: "`codewhale login` 也可以写成 `codewhale account login`。它不接受提供商密钥；密钥请用 `codewhale auth set`。",
        },
      ],
    },
    {
      id: "headless",
      title: "在没有浏览器的机器上使用",
      blocks: [
        {
          p: "在服务器或 CI 中无法用浏览器批准登录。请改用账户 API 密钥：把 `CODEWHALE_API_KEY` 设为以 `cwc_key_` 开头的密钥。你可以在已登录的终端里用 `codewhale account api-keys create --name <name>` 创建它。",
        },
        {
          p: "设置了 `CODEWHALE_API_KEY` 时，作用于整个账户的命令（例如云端项目和智能体）会要求你先取消设置，再运行 `codewhale login`。",
        },
      ],
    },
    {
      id: "phone",
      title: "在手机上登录",
      blocks: [
        {
          p: "手机应用尚未公开，所以目前在手机上没有可登录的地方。",
        },
      ],
    },
    {
      id: "sign-out",
      title: "退出登录",
      blocks: [
        { p: "每个入口只结束它自己的会话，所以在一处退出不会让你在其他地方退出。" },
        {
          rows: [
            ["桌面应用", "在设置的账户页点击退出登录，或点击侧边栏底部你的名字并选择退出登录。"],
            ["终端", "运行 `codewhale account logout`。"],
            ["网页", "在已登录页面点击使用其他账户。"],
          ],
        },
        {
          note: "`codewhale logout` 的作用不同：它会移除所有已保存的提供商密钥和登录，并会先请你确认。只想结束账户会话时，请用 `codewhale account logout`。",
        },
      ],
    },
    {
      id: "data",
      title: "导出数据或删除账户",
      blocks: [
        {
          p: "目前有两个途径。在桌面应用中打开设置、账户、数据与隐私：点击导出我的数据可保存一份副本，点击安排账户删除即可开始删除。30 天后 Codewhale 可以永久删除账户，在删除开始前你可以取消。你电脑上的文件会保留。",
        },
        {
          p: "你也可以在任何入口发邮件到 help@codewhale.net，请求导出或删除。",
        },
      ],
    },
    {
      id: "guide",
      title: "完整的应用指南",
      blocks: [
        {
          p: "桌面应用的分步指南，包括登录时界面显示的所有内容，见 [docs.codewhale.net](https://docs.codewhale.net)。",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/auth",
      label: "连接模型提供商",
      note: "用提供商密钥或本地模型，让 Codewhale 有模型可用。",
    },
    {
      href: "/docs/web",
      label: "打开浏览器客户端",
      note: "在你自己机器的浏览器标签页中使用 Codewhale，无需账户。",
    },
    {
      href: "/docs/trust",
      label: "了解哪些数据会离开本机",
      note: "哪些留在本地、提供商会收到什么，以及如何关闭用量统计。",
    },
  ],
  sourceNote:
    "来源：已发布版本标签下的 crates/cli（login、account status、account logout、logout）、桌面应用的登录流程与设置中的账户页 · 修改时请同步 docs-map.ts。",
  sourceGroups: [
    {
      claim: { feature: "account-login", describesCurrentRow: true },
      sections: [
        {
          id: "source-account-login",
          title: "在终端应用内登录与退出",
          blocks: [
            {
              p: "`/login` 在终端应用内发起账户登录，提供浏览器链接和验证码；`/login status` 显示账户状态。登录会保留你选择的模型和尚未发送的消息。账户连接与可用的模型访问是两回事。",
            },
            {
              p: "`/logout`、`codewhale logout` 和 `codewhale account logout` 只结束账户会话，保留本地模型连接和对话。`codewhale logout --all` 还会移除所有已保存的提供商凭据，并会先请你确认。",
            },
          ],
        },
      ],
    },
  ],
};

export function getDocsAccount(locale: string): DocsPageDict {
  return locale === "zh" ? ZH : EN;
}
