import type { LocalizedText } from "./vocabulary";
import { REPO_ISSUES_URL, REPO_URL } from "../i18n/links";

const DOCS = `${REPO_URL}/blob/main/docs`;
const MILESTONE = `${REPO_URL}/milestone`;
const DISCUSSIONS = `${REPO_URL}/discussions`;

export interface ProjectLink {
  label: LocalizedText;
  href: string;
}

export interface ProjectGoal {
  title: LocalizedText;
  body: LocalizedText;
  open: LocalizedText;
  links: ProjectLink[];
}

const issue = (number: number): ProjectLink => ({
  label: { en: `Issue ${number}`, zh: `Issue ${number}` },
  href: `${REPO_ISSUES_URL}/${number}`,
});

export const PROJECT_GOALS: ProjectGoal[] = [
  {
    title: { en: "One engine behind every surface", zh: "一个引擎，支撑所有入口" },
    body: {
      en: "The terminal, codewhale exec, the local browser view and editors share one Rust engine. A fix there reaches all of them.",
      zh: "终端、codewhale exec、本地浏览器界面和编辑器共用同一个 Rust 引擎。在引擎里修好一处，所有入口都受益。",
    },
    open: {
      en: "Smaller crates, and a desktop app you can download.",
      zh: "把大型 crate 拆小，以及一个可以下载的桌面应用。",
    },
    links: [issue(5316), issue(4986)],
  },
  {
    title: { en: "Every model is a peer", zh: "所有模型一视同仁" },
    body: {
      en: "Use a built-in provider, any OpenAI-compatible endpoint or a local model. No provider is ranked above another.",
      zh: "使用内置提供商、任意 OpenAI 兼容端点或本地模型。没有哪个提供商排在别人前面。",
    },
    open: {
      en: "Provider profiles, and each provider or model you use that nobody has tested yet.",
      zh: "提供商配置档，以及你在用、但还没有人验证过的提供商和模型。",
    },
    links: [issue(5092), { label: { en: "Provider list", zh: "提供商列表" }, href: `${DOCS}/PROVIDERS.md` }],
  },
  {
    title: { en: "You stay in control", zh: "控制权始终在你手里" },
    body: {
      en: "Codewhale asks before it acts outside what you allowed, and records each file, command and call. An unknown cost shows as unknown.",
      zh: "超出你允许的范围时，Codewhale 会先询问，并记录每一个文件、命令和调用。不知道的费用就显示为未知。",
    },
    open: {
      en: "A sandbox on Windows, clearer approval prompts, and your reports of a wrong refusal or a missed question.",
      zh: "Windows 上的沙箱、更清楚的审批提示，以及你遇到的错误拒绝或漏问的情况。",
    },
    links: [{ label: { en: "Sandbox guide", zh: "沙箱说明" }, href: `${DOCS}/SANDBOX.md` }],
  },
  {
    title: { en: "Many agents, one place", zh: "多个 Agent，一处查看" },
    body: {
      en: "Split a job across sub-agents, a Fleet or a Workflow, and follow their progress together.",
      zh: "把一项工作分给子 Agent、Fleet 或 Workflow，并在同一处查看进度。",
    },
    open: {
      en: "Other coding agents running inside Codewhale, long work that survives a restart, and context that holds up in long sessions.",
      zh: "在 Codewhale 里运行其他编码 Agent、重启后仍能继续的长任务，以及在长会话中依然好用的上下文。",
    },
    links: [issue(6013), issue(7206)],
  },
  {
    title: { en: "Bring what you already have", zh: "带上你已有的一切" },
    body: {
      en: "Codewhale reads AGENTS.md, imports Claude Code settings and plugins, and connects MCP servers.",
      zh: "Codewhale 读取 AGENTS.md，导入 Claude Code 的设置和插件，并连接 MCP 服务器。",
    },
    open: {
      en: "DeepSeek Harness plugin compatibility on macOS, Linux and Windows.",
      zh: "在 macOS、Linux 和 Windows 上兼容 DeepSeek Harness 插件。",
    },
    links: [{ label: { en: "Compatibility checklist", zh: "兼容性清单" }, href: `${DOCS}/DSH_PLUGIN_COMPAT.md` }],
  },
  {
    title: { en: "Your language, your machine", zh: "你的语言，你的机器" },
    body: {
      en: "The terminal, the README and this site are translated, and Codewhale runs on macOS, Linux and Windows.",
      zh: "终端、README 和本站都有翻译，Codewhale 可在 macOS、Linux 和 Windows 上运行。",
    },
    open: {
      en: "More translations, input methods, screen readers and new platforms.",
      zh: "更多翻译、输入法、屏幕阅读器和新平台。",
    },
    links: [
      { label: { en: "Localization matrix", zh: "本地化矩阵" }, href: `${DOCS}/LOCALIZATION.md` },
      issue(6804),
    ],
  },
  {
    title: { en: "A project that helps build itself", zh: "一个参与构建自己的项目" },
    body: {
      en: "Codewhale is used to work on Codewhale. A clear issue is something an agent can finish and a person can review.",
      zh: "我们用 Codewhale 来开发 Codewhale。一个写得清楚的 issue，Agent 可以完成，人可以审查。",
    },
    open: {
      en: "Memory across sessions, and skills learned with your approval. Both are at the design stage.",
      zh: "跨会话的记忆，以及经你批准后学会的技能。两者都还在设计阶段。",
    },
    links: [issue(6050), issue(5860)],
  },
];

export type ReleaseState = "progress" | "planned" | "later";

export const RELEASE_STATE_LABEL: Record<ReleaseState, LocalizedText> = {
  progress: { en: "In progress", zh: "进行中" },
  planned: { en: "Planned", zh: "已规划" },
  later: { en: "Later", zh: "更远" },
};

export const PROJECT_RELEASES: { name: string; state: ReleaseState; body: LocalizedText; href: string }[] = [
  {
    name: "0.10.3",
    state: "progress",
    body: {
      en: "Fixes reported against 0.10.2, DeepSeek Harness plugin compatibility, other agents inside Codewhale and a larger Runtime API.",
      zh: "修复针对 0.10.2 报告的问题，兼容 DeepSeek Harness 插件，在 Codewhale 里运行其他 Agent，并扩充 Runtime API。",
    },
    href: `${MILESTONE}/73`,
  },
  {
    name: "0.10.4",
    state: "planned",
    body: {
      en: "Reliability: process cleanup, resource limits and dead-code removal.",
      zh: "可靠性：进程清理、资源限制和死代码移除。",
    },
    href: `${MILESTONE}/75`,
  },
  {
    name: "0.10.5",
    state: "planned",
    body: {
      en: "Usability: running-command detail, usage diagnostics and file previews.",
      zh: "易用性：运行中命令的详情、用量诊断和文件预览。",
    },
    href: `${MILESTONE}/76`,
  },
  {
    name: "0.11",
    state: "planned",
    body: {
      en: "Architecture: smaller crates, provider profiles and one command contract.",
      zh: "架构：更小的 crate、提供商配置档和统一的命令契约。",
    },
    href: `${MILESTONE}/72`,
  },
  {
    name: "1.0",
    state: "later",
    body: {
      en: "The desktop app on every platform, pluggable memory and approved skill learning.",
      zh: "覆盖各平台的桌面应用、可插拔的记忆，以及经批准的技能学习。",
    },
    href: `${MILESTONE}/66`,
  },
];

export const PROJECT_COPY = {
  goalsLabel: { en: "Roadmap", zh: "路线图" },
  goalsHeading: { en: "Where Codewhale is going", zh: "Codewhale 要去哪里" },
  goalsScope: {
    en: "Seven long-term goals. Each names what is still open, because that is where your report or idea changes the outcome.",
    zh: "七个长期目标。每个目标都写明了尚未解决的部分，因为你的报告或想法最能改变那里的结果。",
  },
  goalsListHeading: { en: "Seven long-term goals", zh: "七个长期目标" },
  goalsListScope: {
    en: "Each one names what is still open, because that is where your report or idea changes the outcome.",
    zh: "每个目标都写明了尚未解决的部分，因为你的报告或想法最能改变那里的结果。",
  },
  goalsLink: { en: "Read the roadmap", zh: "查看路线图" },
  goalOpen: { en: "Still open", zh: "尚未解决" },

  releasesHeading: { en: "What each release is for", zh: "每个版本要做什么" },
  releasesScope: {
    en: "A milestone is a plan, not a date. A release exists when its tag and downloads do.",
    zh: "里程碑是计划，不是日期。只有当标签和下载都存在时，版本才算发布。",
  },

  loopHeading: { en: "Codewhale improves through your attention", zh: "Codewhale 因你的关注而变好" },
  loopScope: {
    en: "{count} contributors so far. A bug report, a confusing screen, a translation or a fix: each one moves it.",
    zh: "目前已有 {count} 位贡献者。一份 bug 报告、一个让人困惑的界面、一段翻译或一个修复，都在推动它前进。",
  },
  loop: [
    {
      title: { en: "Use it", zh: "用起来" },
      body: {
        en: "Run Codewhale on your project, with your provider, on your machine.",
        zh: "在你的项目、你的提供商、你的机器上运行 Codewhale。",
      },
    },
    {
      title: { en: "Say what happened", zh: "说说发生了什么" },
      body: {
        en: "Report a bug, an idea or something that looked wrong. You do not need a fix.",
        zh: "报告一个 bug、一个想法，或某处看起来不对劲的地方。不需要附带修复方案。",
      },
    },
    {
      title: { en: "Shape the change", zh: "一起确定方案" },
      body: {
        en: "Agree on a feature in an issue, or send a fix, a translation or a provider.",
        zh: "在 issue 里把功能商量清楚，或直接提交修复、翻译或提供商。",
      },
    },
    {
      title: { en: "See it ship", zh: "看着它发布" },
      body: {
        en: "Your pull request merges under your name, and the release notes credit you.",
        zh: "你的 pull request 以你的名义合并，发布说明里会写上你的名字。",
      },
    },
  ],
  loopFeedback: { en: "Give feedback", zh: "提交反馈" },
  loopHelp: { en: "See where to help", zh: "看看哪里需要帮助" },

  routesHeading: { en: "Find where yours goes", zh: "找到合适的去处" },
  routesScope: {
    en: "Not sure? Open an issue. A maintainer moves it if needed.",
    zh: "拿不准？直接提交 issue，维护者会在需要时帮你转移。",
  },
  featureLabel: { en: "New features", zh: "新功能" },
  featureHeading: { en: "Start a feature with an issue", zh: "新功能从 issue 开始" },
  featureScope: {
    en: "The questions that decide a feature are about the product, not the diff. Agreeing first keeps your pull request small and quick to merge.",
    zh: "决定一个功能的问题在于产品，而不在于 diff。先达成一致，你的 pull request 才能小而快地合并。",
  },
  featureSteps: [
    {
      title: { en: "Open a feature request", zh: "提交功能请求" },
      body: {
        en: "Say what you are trying to do and what gets in the way. A design is welcome and optional.",
        zh: "说明你想做什么、被什么挡住了。欢迎附上设计，但不是必须。",
      },
    },
    {
      title: { en: "Agree on the shape", zh: "确定方案" },
      body: {
        en: "A maintainer answers with what fits, or with what is still undecided.",
        zh: "维护者会回复合适的方案，或说明哪些地方还没有定论。",
      },
    },
    {
      title: { en: "Send the pull request", zh: "提交 pull request" },
      body: {
        en: "Link the issue. A feature pull request with no issue is still read, and it takes longer.",
        zh: "关联对应的 issue。没有 issue 的功能 pull request 我们同样会看，只是会更慢。",
      },
    },
  ],
  standingHeading: { en: "Send these without asking", zh: "这些可以直接提交" },
  standingScope: {
    en: "Standing projects are always open. A pull request needs no issue first.",
    zh: "长期项目始终开放，提交 pull request 前不需要先开 issue。",
  },
  expectLabel: { en: "In return", zh: "你会得到" },
  expectHeading: { en: "What you get back", zh: "你会得到的回应" },
  expect: [
    {
      title: { en: "A specific reply", zh: "具体的回复" },
      body: {
        en: "It names what you reported, diagnosed or fixed.",
        zh: "回复会说明你报告、诊断或修复了什么。",
      },
    },
    {
      title: { en: "Your name on the change", zh: "改动署你的名" },
      body: {
        en: "Your pull request merges as itself, with you as the author.",
        zh: "你的 pull request 按原样合并，作者就是你。",
      },
    },
    {
      title: { en: "No rebase chores", zh: "不用反复 rebase" },
      body: {
        en: "You do not rebase around maintainer changes, and a red check you did not cause is not yours to fix.",
        zh: "你不必因为维护者的改动而 rebase，不是你造成的失败检查也不需要你来修。",
      },
    },
    {
      title: { en: "Credit in each release", zh: "每个版本都有署名" },
      body: {
        en: "The changelog and the contributors list carry your handle.",
        zh: "更新日志和贡献者名单会写上你的 GitHub 用户名。",
      },
    },
  ],
  roadmapHeading: { en: "See where the project is headed", zh: "看看项目的方向" },
  statusLabel: { en: "Status", zh: "状态" },
};

export interface ContributionRoute {
  have: LocalizedText;
  place: LocalizedText;
  href: string;
}

const NEW_ISSUE = `${REPO_ISSUES_URL}/new`;

export const CONTRIBUTION_ROUTES: ContributionRoute[] = [
  {
    have: { en: "A bug, or behavior that surprised you", zh: "一个 bug，或出乎意料的行为" },
    place: { en: "Bug report", zh: "Bug 报告" },
    href: `${NEW_ISSUE}?template=bug_report.yml`,
  },
  {
    have: { en: "An idea or a feature you want", zh: "一个想法，或你想要的功能" },
    place: { en: "Feature request", zh: "功能请求" },
    href: `${NEW_ISSUE}?template=feature_request.yml`,
  },
  {
    have: { en: "Feedback on how it looks, reads or feels", zh: "对外观、文字或使用感受的反馈" },
    place: { en: "Feedback form", zh: "反馈表单" },
    href: `${NEW_ISSUE}?template=feedback.yml`,
  },
  {
    have: { en: "A question", zh: "一个问题" },
    place: { en: "Q&A or Discord", zh: "问答区或 Discord" },
    href: `${DISCUSSIONS}/categories/q-a`,
  },
  {
    have: { en: "A security problem", zh: "一个安全问题" },
    place: { en: "Private advisory", zh: "私密安全报告" },
    href: `${REPO_URL}/security/advisories/new`,
  },
  {
    have: { en: "A fix, a translation, a provider or a docs change", zh: "修复、翻译、提供商或文档改动" },
    place: { en: "Pull request", zh: "Pull request" },
    href: `${REPO_URL}/blob/main/CONTRIBUTING.md#standing-projects`,
  },
  {
    have: { en: "A new feature", zh: "一个新功能" },
    place: { en: "Issue first", zh: "先开 issue" },
    href: `${NEW_ISSUE}?template=feature_request.yml`,
  },
];

export const STANDING_PROJECTS: { title: LocalizedText; body: LocalizedText; href: string }[] = [
  {
    title: { en: "Translations", zh: "翻译" },
    body: {
      en: "Improve the terminal language packs, the translated READMEs, this site or the docs.",
      zh: "改进终端语言包、各语言 README、本站或文档的翻译。",
    },
    href: `${DOCS}/LOCALIZATION.md`,
  },
  {
    title: { en: "Providers and models", zh: "提供商与模型" },
    body: {
      en: "Add a provider or a model, or correct a price, a context size or a provider quirk.",
      zh: "添加提供商或模型，或纠正价格、上下文大小和提供商的特殊行为。",
    },
    href: `${DOCS}/PROVIDERS.md`,
  },
  {
    title: { en: "Documentation", zh: "文档" },
    body: {
      en: "Fix a typo, an unclear step or an instruction that no longer matches the product.",
      zh: "修正错别字、不清楚的步骤，或与产品不再一致的说明。",
    },
    href: `${REPO_URL}/tree/main/docs`,
  },
  {
    title: { en: "Bug fixes", zh: "Bug 修复" },
    body: {
      en: "No issue yet? Describe the bug in the pull request: what you ran, what happened, what you expected.",
      zh: "还没有 issue？在 pull request 里描述这个 bug：你运行了什么、发生了什么、你期望什么。",
    },
    href: `${REPO_ISSUES_URL}?q=is%3Aissue+is%3Aopen+label%3Abug`,
  },
  {
    title: { en: "Planned work", zh: "已规划的工作" },
    body: {
      en: "Pick an issue labelled help wanted or one in a milestone. Comment to claim it.",
      zh: "选择带 help wanted 标签或属于某个里程碑的 issue，留言认领。",
    },
    href: `${REPO_ISSUES_URL}?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22`,
  },
];
