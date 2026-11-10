import Link from "next/link";
import { Icon } from "@/components/icon";
import { PageHeader, Section } from "@/components/page-header";
import { Status, type StatusTone } from "@/components/status-badge";
import { getCachedRoadmap, type RoadmapItem } from "@/lib/roadmap-feed";
import { fetchMilestones, type MilestoneProgress } from "@/lib/github";
import { getEnv } from "@/lib/kv";
import { GoalList } from "@/components/goal-list";
import { PROJECT_COPY, PROJECT_RELEASES, RELEASE_STATE_LABEL, type ReleaseState } from "@/lib/content/project";
import { fill, getRoadmap, pickText, pickTextLocale } from "@/lib/i18n/dictionaries";
import { buildPageMetadata } from "@/lib/page-meta";

export const revalidate = 1800;

export async function generateMetadata({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getRoadmap(locale);
  return buildPageMetadata({
    path: "/roadmap",
    locale,
    title: t.metaTitle,
    description: t.metaDescription,
  });
}

type TrackItem = { title: string; note: string; href?: string };
type Track = { title: string; items: TrackItem[] };

const tracksEn: Track[] = [
  {
    title: "Shipped",
    items: [
      { title: "Small searchable toolbox", note: "Every turn starts with a small pinned set — read, write, edit, file_search, bash, agent, workflow, todo and goal controls, and skills — plus synthetic tool_search. Specialized native, Web, MCP, plugin, memory, task, and verification tools are policy-filtered and searchable; activated schemas stay in a bounded per-conversation toolbox." },
      { title: "Sub-agent parallel execution", note: "agent; 64 concurrent sessions by default, configurable to 128, with bounded result handles" },
      { title: "RLM batched processing", note: "Persistent sandboxed Python REPL with 1–16 cheap parallel children for long-input analysis" },
      { title: "Three operating modes", note: "Plan (read-only authority), Work (execution), and Operate (Fleet/Workflow orchestration) keep one primitive vocabulary; Ask / Auto-Review / Full Access remains an orthogonal approval posture." },
      { title: "OS command sandbox", note: "Seatbelt on macOS when available; opt-in bubblewrap on Linux when installed. Windows currently reports no OS sandbox." },
      { title: "Durable sessions + tasks", note: "Save, resume, rollback; background task queue with replayable timelines" },
      { title: "Bidirectional MCP", note: "Consume tools from external servers; expose as a server via `codewhale serve --mcp`" },
      { title: "Skills + unified slash palette", note: "Skills load from trusted roots; /help, /mode, /status, /config, /trust, /feedback" },
      { title: "OpenRouter provider", note: "OpenRouter integration with 300+ models across dozens of providers" },
      { title: "OpenAI-compatible & local runtimes", note: "Generic `openai` route for any OpenAI-compatible gateway, plus vLLM, SGLang, and Ollama against your own localhost endpoints — no key required" },
      { title: "Multi-provider support", note: "Hot-swap between providers (DeepSeek, OpenAI, Anthropic, OpenRouter) per session" },
      { title: "Local web client", note: "In published releases: `codewhale web` is a loopback-only browser client over the Runtime API behind a one-time bootstrap session boundary; approvals and user input recover across page reloads" },
      { title: "Anonymous usage counting", note: "On by default and disclosed at first launch. Turning it off is a saved choice that later versions keep, and showing the notice never records any acceptance on your behalf. While on, sessions post aggregate session, feature, and error counts to a first-party endpoint whose full source is in the repository; it stores no IP, country, or geo data and keeps records for three months. Never conversations, code, prompts, files, file/repo/branch names, model content, credentials, or a per-turn/per-tool timeline. Turn it off with `codewhale config set telemetry false` or CODEWHALE_TELEMETRY=0." },
    ],
  },
  {
    title: "Underway",
    items: [
      { title: "VS Code extension", note: "The repository ships an early local-runtime companion: terminal launch, health checks, read-only thread summaries, and restore-point browsing. Full chat and editor actions are not part of this slice." },
      { title: "Memory typed store", note: "The native store has shipped: Markdown is the durable source of truth, with a rebuildable SQLite FTS5 index over it. Graph-structured agent memory and multi-signal recall are still ahead" },
      { title: "Feishu / Lark bot", note: "First long-connection bridge over the runtime API shipped; richer chat features underway" },
      { title: "Chinese-market & i18n", note: "Locale-aware UI, platform refinements, region-specific search backends" },
      { title: "Hugging Face model discovery + Model Lab", note: "Browse, download, and manage models from Hugging Face Hub directly in the TUI" },
    ],
  },
  {
    title: "Considered",
    items: [
      { title: "Workrooms", note: "Durable, addressable agent-work threads over the Runtime API and user surfaces" },
      { title: "Exa web-search backend", note: "Bundled alternative to the existing DDG + Bing path" },
      { title: "Homebrew core formula", note: "Tap exists; pursuing homebrew-core inclusion" },
      { title: "Native Windows installer", note: "MSI installer; the WinGet package, Scoop manifest, and NSIS setup already ship" },
      { title: "Unsloth / NeMo / Arcee fine-tune integration", note: "One-click fine-tuning workflows backed by Unsloth, NVIDIA NeMo, and Arcee toolkits" },
    ],
  },
  {
    title: "Ruled out",
    items: [
      { title: "Silent or content-level telemetry", note: "Codewhale counts anonymous usage by default, discloses it at first launch, and keeps every opt-out. What stays ruled out: conversations, code, prompts, files, model content, credentials, per-turn or per-tool timelines, collection hidden from the user, and any third-party ad or analytics SDK in the runtime binary. A selected hosted provider still receives the context required for its model turn; loopback routes can keep inference local." },
      { title: "Mandatory hosted relay for local sessions", note: "The local runtime and bring-your-own-provider routes continue to work without sending sessions through a Codewhale service" },
      { title: "Required account for the local runtime", note: "Installing and running Codewhale locally requires no account" },
      { title: "Sponsored model promotion", note: "Model picker stays neutral — no paid placement" },
      { title: "Public share links for local sessions", note: "The retired share-link direction is not coming back; any future sharing design starts fresh." },
    ],
  },
  {
    title: "Open model platform · ideas, not started",
    items: [
      { title: "Community model registry", note: "Discover, share, and rate community fine-tuned models with reproducible recipes" },
      { title: "One-click deploy", note: "Deploy any model to RunPod, Replicate, or your own infra with a single command" },
      { title: "Model evaluation dashboard", note: "Transparent, reproducible comparisons across providers, quantization levels, and hardware" },
    ],
  },
];

const tracksZh: Track[] = [
  {
    title: "已完成",
    items: [
      { title: "小型可搜索工具箱", note: "每轮以一组固定的核心工具开始——read、write、edit、file_search、bash、agent、workflow、todo 与目标控制、技能——外加合成的 tool_search。原生专用工具、Web、MCP、插件、记忆、任务和验证工具按策略过滤并可搜索；激活的 schema 保存在有界的会话工具箱中。" },
      { title: "子 Agent 并行执行", note: "agent；默认 64 个并发会话，可配置到 128 个，通过 var_handle 有界读取结果" },
      { title: "RLM 批量处理", note: "持久沙箱 Python REPL，支持 1–16 路廉价并行子调用，处理长文本分析" },
      { title: "三种运行模式", note: "Plan（只读权限）、Work（执行）与 Operate（Fleet / Workflow 编排）使用同一套基础工具名称；Ask、Auto-Review 与 Full Access 审批姿态独立设置。" },
      { title: "OS 命令沙箱", note: "macOS 在可用时使用 Seatbelt；Linux 在安装后可显式启用 bubblewrap。Windows 当前报告无 OS 沙箱。" },
      { title: "持久化会话 + 后台任务", note: "保存、恢复、回滚；后台任务队列，可回放时间线" },
      { title: "双向 MCP 协议", note: "消费外部服务器工具；通过 `codewhale serve --mcp` 暴露为服务器" },
      { title: "技能 + 统一命令面板", note: "技能从 Codewhale 目录自动加载；/help、/mode、/status、/config、/trust、/feedback" },
      { title: "OpenRouter 提供商", note: "原生集成 OpenRouter，支持 300+ 模型，覆盖数十个提供商" },
      { title: "OpenAI 兼容与本地运行时", note: "通用 `openai` 路由可接入任意 OpenAI 兼容网关；vLLM、SGLang、Ollama 直连本地端点，无需密钥" },
      { title: "多提供商支持", note: "按会话动态切换提供商（DeepSeek、OpenAI、Anthropic、OpenRouter）" },
      { title: "本地 Web 客户端", note: "已在发布版本中提供：`codewhale web` 是基于 Runtime API 与一次性引导会话边界的回环地址浏览器客户端；审批与用户输入可在页面刷新后恢复" },
      { title: "匿名使用计数", note: "默认开启，并在首次启动时告知。关闭是会被后续版本保留的选择，显示告知绝不会代你记录任何同意。开启时，会话只会把聚合的会话、功能与错误计数发送到第一方端点，其完整源码在仓库中；它不存储 IP、国家或地理位置，记录保留三个月。永远不收集对话、代码、prompt、文件、文件/仓库/分支名、模型内容、凭据，也不发送逐轮或逐工具时间线。用 `codewhale config set telemetry false` 或 CODEWHALE_TELEMETRY=0 关闭。" },
    ],
  },
  {
    title: "进行中",
    items: [
      { title: "VS Code 扩展", note: "仓库已提供早期本地 Runtime 配套扩展：终端启动、健康检查、只读线程摘要和还原点浏览；完整聊天与编辑器操作尚未包含在此版本中。" },
      { title: "记忆类型化存储", note: "原生存储已发布：以 Markdown 作为持久化事实来源，并在其上建立可重建的 SQLite FTS5 索引。图结构 Agent 记忆与多信号召回仍在规划中" },
      { title: "飞书 / Lark 机器人", note: "基于 runtime API 的长连接桥接已发布首版；更丰富的对话能力进行中" },
      { title: "中国市场与国际化改进", note: "本地化 UI、平台优化、区域搜索引擎" },
      { title: "Hugging Face 模型发现 + 模型实验室", note: "在 TUI 中直接浏览、下载和管理 Hugging Face Hub 上的模型" },
    ],
  },
  {
    title: "考虑中",
    items: [
      { title: "Workrooms 工作间", note: "基于 Runtime API 与用户界面的持久、可寻址 Agent 工作线程" },
      { title: "Exa 网页搜索后端", note: "内建替代 DDG + Bing 的搜索路由" },
      { title: "Homebrew 核心仓库", note: "Tap 已有；正在争取进入 homebrew-core" },
      { title: "Windows 原生安装器", note: "MSI 安装器；WinGet 包、Scoop 清单与 NSIS 安装程序均已发布" },
      { title: "Unsloth / NeMo / Arcee 微调集成", note: "一键微调工作流，由 Unsloth、NVIDIA NeMo 和 Arcee 工具链驱动" },
    ],
  },
  {
    title: "暂不考虑",
    items: [
      { title: "静默或内容级遥测", note: "Codewhale 默认开启匿名使用计数，并在首次启动时告知；所有关闭选项始终有效。仍被排除在外的是：对话、代码、prompt、文件、模型内容、凭据、逐轮或逐工具时间线、对用户隐藏的收集，以及运行时二进制中的任何第三方广告或分析 SDK。选用托管 provider 时仍会发送本轮所需上下文，回环地址路由可让推理保持本地。" },
      { title: "本地会话强制经过托管中继", note: "本地 Runtime 与自带提供商路由继续工作，无需把会话发送到 Codewhale 服务" },
      { title: "本地 Runtime 强制注册账户", note: "本地安装和运行 Codewhale 不需要账户" },
      { title: "赞助商模型推广", note: "模型选择器保持中立——无付费推荐位" },
      { title: "本地会话的公开分享链接", note: "已停用的分享链接方向不会恢复；未来的分享设计将重新开始。" },
    ],
  },
  {
    title: "开放模型平台 · 构想，尚未开始",
    items: [
      { title: "社区模型注册中心", note: "发现、分享和评价社区微调模型，附带可复现的配方" },
      { title: "一键部署", note: "一条命令将任意模型部署到 RunPod、Replicate 或自有基础设施" },
      { title: "模型评测面板", note: "跨提供商、量化级别和硬件的透明、可复现对比" },
    ],
  },
];

// Track order is shared by both languages: shipped, underway, considered,
// ruled out, and the open model platform direction (not started).
const TRACK_TONES: StatusTone[] = ["ready", "accent", "idle", "idle", "idle"];
const RELEASE_TONES: Record<ReleaseState, StatusTone> = { progress: "accent", planned: "idle", later: "idle" };

const roadmapText = (text: string) =>
  text.replace(/^>\s*/, "").replaceAll("**", "").replaceAll("CodeWhale", "Codewhale");

export default async function RoadmapPage({ params }: { params: Promise<{ locale: string }> }) {
  const { locale } = await params;
  const t = getRoadmap(locale);
  const baseTracks = { en: tracksEn, zh: tracksZh }[pickTextLocale(locale)];

  // Live feed: shipped from GitHub Releases; underway/considered/ruled-out from issue labels.
  // Per-category fallback to the static items so unlabeled categories stay populated.
  let tracks: Track[] = baseTracks;
  try {
    const env = await getEnv();
    const feed = await getCachedRoadmap(env.CURATED_KV, env.GITHUB_TOKEN);
    if (feed) {
      const liveByCategory: Record<string, RoadmapItem[]> = {
        Shipped: feed.shipped,
        Underway: feed.underway,
        Considered: feed.considered,
        "Ruled out": feed.ruledOut,
        已完成: feed.shipped,
        进行中: feed.underway,
        考虑中: feed.considered,
        暂不考虑: feed.ruledOut,
      };
      tracks = baseTracks.map((t) => {
        const live = liveByCategory[t.title];
        if (live && live.length > 0) {
          return { ...t, items: live.map((it) => ({ title: it.title, note: it.note, href: it.href })) };
        }
        return t;
      });
    }
  } catch {
    /* keep static fallback */
  }

  let milestones: MilestoneProgress[] = [];
  try {
    milestones = await fetchMilestones((await getEnv()).GITHUB_TOKEN);
  } catch {
    milestones = [];
  }
  const progressFor = (href: string) => milestones.find((m) => href.endsWith(`/${m.number}`));

  const links = [
    { title: "Issues", detail: t.issuesDetail, href: "https://github.com/codewhale-hq/CodeWhale/issues" },
    { title: pickText(PROJECT_COPY.featureSteps[0].title, locale), detail: pickText(PROJECT_COPY.featureSteps[0].body, locale), href: "https://github.com/codewhale-hq/CodeWhale/issues/new?template=feature_request.yml" },
    { title: "Pull requests", detail: t.pullsDetail, href: "https://github.com/codewhale-hq/CodeWhale/pulls" },
  ];

  return (
    <>
      <PageHeader
        kicker={t.eyebrow}
        title={t.title}
        lede={t.introduction}
        pose="browse"
        actions={
          <>
            <Link href={`/${locale}/contribute`} className="btn btn-primary btn-lg">
              {pickText(PROJECT_COPY.loopHelp, locale)}
              <Icon name="arrow-right" className="icon icon-flip" />
            </Link>
            <Link href="https://github.com/codewhale-hq/CodeWhale/blob/main/docs/ROADMAP.md" className="btn btn-secondary btn-lg">
              GitHub
              <Icon name="external" className="icon" />
            </Link>
          </>
        }
      />

      <div className="page-body">
        <Section
          id="roadmap-goals"
          title={pickText(PROJECT_COPY.goalsListHeading, locale)}
          scope={pickText(PROJECT_COPY.goalsListScope, locale)}
        >
          <GoalList locale={locale} detail />
        </Section>

        <Section
          id="roadmap-releases"
          title={pickText(PROJECT_COPY.releasesHeading, locale)}
          scope={pickText(PROJECT_COPY.releasesScope, locale)}
        >
          <dl className="ruled-list release-plan">
            {PROJECT_RELEASES.map((release) => (
              <div key={release.name}>
                <dt>
                  <Link href={release.href} className="tabular">{release.name}</Link>
                </dt>
                <dd>
                  <Status tone={RELEASE_TONES[release.state]}>{pickText(RELEASE_STATE_LABEL[release.state], locale)}</Status>
                  {pickText(release.body, locale)}
                  {(() => {
                    const progress = progressFor(release.href);
                    const total = progress ? progress.open + progress.closed : 0;
                    if (!progress || total === 0) return null;
                    return (
                      <span className="tide-progress">
                        <span className="tide-track" aria-hidden="true"><span style={{ inlineSize: `${Math.round((progress.closed / total) * 100)}%` }} /></span>
                        <span className="tabular">{fill(pickText(PROJECT_COPY.milestoneProgress, locale), { closed: progress.closed, total })}</span>
                      </span>
                    );
                  })()}
                </dd>
              </div>
            ))}
          </dl>
        </Section>

        <Section
          id="roadmap-status"
          title={t.sectionTitle}
          link={
            <Link href="https://github.com/codewhale-hq/CodeWhale/issues" className="section-link">
              {t.browseIssues}
              <Icon name="external" className="icon" />
            </Link>
          }
        >
          <div className="roadmap-tracks">
            {tracks.map((track, index) => (
              <section key={track.title} className="roadmap-track" aria-labelledby={`track-${index}`}>
                <div className="roadmap-track-head">
                  <h3 id={`track-${index}`}>
                    <Status tone={TRACK_TONES[index] ?? "idle"}>{track.title}</Status>
                  </h3>
                  <span className="page-meta tabular">{fill(track.items.length === 1 ? t.trackCountOne : t.trackCount, { count: track.items.length })}</span>
                </div>
                <ul className="group-card" role="list">
                  {track.items.map((item) => (
                    <li key={`${item.title}-${item.note}`} className="group-row roadmap-item">
                      <p className="roadmap-item-title">
                        {item.href ? <Link href={item.href}>{roadmapText(item.title)}</Link> : roadmapText(item.title)}
                      </p>
                      <p className="roadmap-item-note">{roadmapText(item.note)}</p>
                    </li>
                  ))}
                </ul>
              </section>
            ))}
          </div>
        </Section>

        <Section id="roadmap-contribute" title={t.contributeTitle} scope={t.contributeBody}>
          <ul className="dir-list dir-list-card" role="list">
            {links.map((link) => (
              <li key={link.title}>
                <Link href={link.href} className="dir-row">
                  <span className="dir-mark" aria-hidden="true">
                    <Icon name={link.title === "Pull requests" ? "git-pull-request" : link.title === "Discussions" ? "message" : "alert"} />
                  </span>
                  <span className="dir-text">
                    <span className="dir-title">{link.title}</span>
                    <span className="dir-purpose">{link.detail}</span>
                  </span>
                  <span className="dir-action" aria-hidden="true"><Icon name="external" /></span>
                </Link>
              </li>
            ))}
          </ul>
        </Section>
      </div>
    </>
  );
}
