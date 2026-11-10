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
      en: "The largest crates still need to be split, and the desktop app has no public download yet.",
      zh: "最大的几个 crate 还需要拆分，桌面应用也还没有公开下载。",
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
      en: "Provider behavior still needs to move into profiles, and many providers and models that people use have not been tested by anyone yet.",
      zh: "提供商的行为还需要整理成配置档，而且很多大家在用的提供商和模型还没有人验证过。",
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
      en: "Windows still has no sandbox, approval prompts could be clearer, and every report of a wrong refusal or a missed question helps.",
      zh: "Windows 上还没有沙箱，审批提示还可以更清楚，每一份关于错误拒绝或漏问的报告都有帮助。",
    },
    links: [{ label: { en: "Sandbox guide", zh: "沙箱说明" }, href: `${DOCS}/SANDBOX.md` }],
  },
  {
    title: { en: "Many agents work in one place", zh: "多个 Agent 在同一处协作" },
    body: {
      en: "Split a job across sub-agents, a Fleet or a Workflow, and follow their progress together.",
      zh: "把一项工作分给子 Agent、Fleet 或 Workflow，并在同一处查看进度。",
    },
    open: {
      en: "Other coding agents cannot run inside Codewhale yet, long work does not always survive a restart, and context needs to hold up better in long sessions.",
      zh: "其他编码 Agent 还不能在 Codewhale 里运行，长任务在重启后不一定能继续，长会话中的上下文也需要更可靠。",
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
      en: "DeepSeek Harness plugins are not yet fully compatible on macOS, Linux and Windows.",
      zh: "DeepSeek Harness 插件在 macOS、Linux 和 Windows 上还没有完全兼容。",
    },
    links: [{ label: { en: "Compatibility checklist", zh: "兼容性清单" }, href: `${DOCS}/DSH_PLUGIN_COMPAT.md` }],
  },
  {
    title: { en: "It works in your language on your machine", zh: "它使用你的语言，运行在你的机器上" },
    body: {
      en: "The terminal, the README and this site are translated, and Codewhale runs on macOS, Linux and Windows.",
      zh: "终端、README 和本站都有翻译，Codewhale 可在 macOS、Linux 和 Windows 上运行。",
    },
    open: {
      en: "More translations are needed, and input methods, screen readers and new platforms all need people who use them.",
      zh: "还需要更多翻译，输入法、屏幕阅读器和新平台也都需要实际使用它们的人来帮忙。",
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
      en: "Memory across sessions and skills learned with your approval are both still at the design stage.",
      zh: "跨会话的记忆和经你批准后学会的技能，都还在设计阶段。",
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
      en: "This release fixes problems reported against 0.10.2 and adds DeepSeek Harness plugin compatibility, other agents inside Codewhale and a larger Runtime API.",
      zh: "这个版本修复针对 0.10.2 报告的问题，并加入 DeepSeek Harness 插件兼容、在 Codewhale 里运行其他 Agent 以及更完整的 Runtime API。",
    },
    href: `${MILESTONE}/73`,
  },
  {
    name: "0.10.4",
    state: "planned",
    body: {
      en: "This release improves reliability through process cleanup, resource limits and dead-code removal.",
      zh: "这个版本通过进程清理、资源限制和移除死代码来提高可靠性。",
    },
    href: `${MILESTONE}/75`,
  },
  {
    name: "0.10.5",
    state: "planned",
    body: {
      en: "This release improves usability with running-command detail, usage diagnostics and file previews.",
      zh: "这个版本通过运行中命令的详情、用量诊断和文件预览来改善易用性。",
    },
    href: `${MILESTONE}/76`,
  },
  {
    name: "0.11",
    state: "planned",
    body: {
      en: "This release reworks the architecture with smaller crates, provider profiles and one command contract.",
      zh: "这个版本重整架构，包括更小的 crate、提供商配置档和统一的命令契约。",
    },
    href: `${MILESTONE}/72`,
  },
  {
    name: "1.0",
    state: "later",
    body: {
      en: "This release brings the desktop app to every platform and adds pluggable memory and approved skill learning.",
      zh: "这个版本把桌面应用带到各个平台，并加入可插拔的记忆和经批准的技能学习。",
    },
    href: `${MILESTONE}/66`,
  },
];

export const PROJECT_COPY = {
  goalsLabel: { en: "Roadmap", zh: "路线图" },
  goalsHeading: { en: "Where Codewhale is going", zh: "Codewhale 要去哪里" },
  goalsScope: {
    en: "Codewhale has seven long-term goals, and each one names what is still open because that is where your report or idea changes the outcome.",
    zh: "Codewhale 有七个长期目标，每个目标都写明了尚未解决的部分，因为你的报告或想法最能改变那里的结果。",
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

  liveLabel: { en: "Live from GitHub", zh: "来自 GitHub 的实时数据" },
  liveHeading: { en: "Meet the people building Codewhale", zh: "认识构建 Codewhale 的人" },
  liveScope: {
    en: "Codewhale improves through the people who use it and say what happened. These are their latest changes.",
    zh: "Codewhale 靠使用它并说出实际情况的人而不断变好。这些是他们最近的改动。",
  },
  liveLink: { en: "All activity", zh: "全部动态" },
  mergedHeading: { en: "Just merged", zh: "刚刚合并" },
  mergedAge: { en: "merged {age}", zh: "{age}合并" },
  firstContribution: { en: "First contribution", zh: "首次贡献" },
  statStars: { en: "Stars", zh: "Star" },
  statContributors: { en: "Contributors", zh: "贡献者" },
  statPulls: { en: "Open pull requests", zh: "开放的 pull request" },
  statIssues: { en: "Open issues", zh: "开放的 issue" },
  facesAll: { en: "All {count} contributors", zh: "全部 {count} 位贡献者" },
  loopFeedback: { en: "Give feedback", zh: "提交反馈" },
  loopHelp: { en: "See where to help", zh: "看看哪里需要帮助" },

  mergedPageHeading: { en: "Merged in the last few days", zh: "最近几天合并的改动" },
  mergedPageScope: {
    en: "Each of these pull requests merged under its author's name.",
    zh: "这里的每个 pull request 都以作者本人的名义合并。",
  },
  openNowHeading: { en: "Pick up something open now", zh: "现在就可以认领的工作" },
  openNowScope: {
    en: "These issues carry the help wanted label on GitHub right now, and you can comment on one to claim it.",
    zh: "这些 issue 现在在 GitHub 上带有 help wanted 标签，你可以留言认领其中一个。",
  },
  milestoneProgress: { en: "{closed} of {total} closed", zh: "已关闭 {closed} / {total}" },

  routesHeading: { en: "Find where yours goes", zh: "找到合适的去处" },
  routesScope: {
    en: "If you are not sure, open an issue and a maintainer will move it where it belongs.",
    zh: "如果拿不准，就直接提交 issue，维护者会把它转到合适的地方。",
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
        en: "Link the issue in your pull request, because a feature pull request with no issue is still read but takes longer.",
        zh: "在 pull request 中关联对应的 issue，因为没有 issue 的功能 pull request 我们同样会看，但会更慢。",
      },
    },
  ],
  standingHeading: { en: "Send these without asking", zh: "这些可以直接提交" },
  standingScope: {
    en: "These standing projects are always open, and a pull request for them needs no issue first.",
    zh: "这些长期项目始终开放，为它们提交 pull request 前不需要先开 issue。",
  },
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
      en: "If there is no issue yet, describe the bug in the pull request by saying what you ran, what happened and what you expected.",
      zh: "如果还没有 issue，就在 pull request 里描述这个 bug，说明你运行了什么、发生了什么以及你期望什么。",
    },
    href: `${REPO_ISSUES_URL}?q=is%3Aissue+is%3Aopen+label%3Abug`,
  },
  {
    title: { en: "Planned work", zh: "已规划的工作" },
    body: {
      en: "Pick an issue labelled help wanted or one in a milestone, and comment on it to claim it.",
      zh: "选择带 help wanted 标签或属于某个里程碑的 issue，并留言认领。",
    },
    href: `${REPO_ISSUES_URL}?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22`,
  },
];
