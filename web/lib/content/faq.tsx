import Link from "next/link";
import { AUTO_ROUTING_COPY } from "./faq-routing";
import { GETTING_STARTED_STEPS } from "./getting-started";
import { FACTS } from "../facts.generated";
import { pickText } from "../i18n/dictionaries";

interface FaqItem {
  q: string;
  a: React.ReactNode;
  sources?: string[];
}

/** `p` prefixes a site path with the reader's locale. */
type SitePath = (path: string) => string;

export const faqEn = (p: SitePath): FaqItem[] => [
  {
    q: "What is Codewhale?",
    a: (
      <>
        Codewhale is an open-source agent for building apps and tools, automating workflows, and working across files, terminal commands and connected apps. Add tools through plugins and MCP servers, or use APIs from your own scripts. Each service needs its own setup and authentication. Use the model APIs you already have or local and self-hosted inference. Start it with <code className="inline">codewhale</code> and choose the access and approvals for its work. DeepSeek is the bundled default route; the model and provider are yours to choose.
      </>
    ),
    sources: ["README.md", "docs/GUIDE.md", "docs/PLUGINS.md", "docs/MCP.md", "docs/PROVIDERS.md"],
  },
  {
    q: "How do I install Codewhale?",
    a: (
      <>
        <p className="mb-2">Published channels differ in timing and platform support:</p>
        <pre tabIndex={0} className="code-block mb-2">
{`# GitHub release binaries (recommended on macOS / Linux)
${GETTING_STARTED_STEPS[0].commands[0]}

# npm alternative — no Rust toolchain needed
npm install -g codewhale

# Cargo (needs Rust 1.89+; installs the codewhale command)
cargo install codewhale-cli --locked

# Homebrew on Linux (tap; tested on Ubuntu)
brew install Hmbown/deepseek-tui/codewhale

# Direct download
# https://github.com/codewhale-hq/CodeWhale/releases`}
        </pre>
        <p>
          Run <code className="inline">codewhale</code> to start. First run creates <code className="inline">~/.codewhale/</code> automatically. Legacy <code className="inline">~/.deepseek/</code> is still read as a compatibility fallback.
          Android arm64 / Termux is preview support: npm works only when the
          selected package version has matching Android assets in its GitHub Release.
          See the <Link href={p("/install")} className="body-link">full install guide</Link> for China mirrors, Docker, and troubleshooting.
        </p>
      </>
    ),
    sources: ["README.md", "docs/INSTALL.md", "#1860", "#1914"],
  },
  {
    q: "Is Codewhale free? Do I need an account?",
    a: (
      <>
        Codewhale is free and MIT licensed, and the terminal app needs no account. With your own API key you pay the provider directly; local models through Ollama, vLLM or SGLang need no key.
        Keys you add with <code className="inline">/provider</code> stay on your machine. The footer and <code className="inline">/cost</code> show what a session has cost when the price is known, and an unknown price is shown as unknown, never as free.
      </>
    ),
    sources: ["README.md", "LICENSE"],
  },
  {
    q: "I use Claude Code, Cursor, or Copilot. What carries over?",
    a: (
      <>
        Codewhale reads <code className="inline">AGENTS.md</code> on its own. Instruction files written for other tools, such as <code className="inline">CLAUDE.md</code>, <code className="inline">.cursorrules</code> or <code className="inline">.github/copilot-instructions.md</code>, load only when you opt in with <code className="inline">project_instruction_imports = [&quot;claude&quot;]</code> (or <code className="inline">&quot;cursor&quot;</code>, <code className="inline">&quot;cline&quot;</code>, <code className="inline">&quot;windsurf&quot;</code>, <code className="inline">&quot;copilot&quot;</code>, <code className="inline">&quot;all&quot;</code>), and Codewhale warns when it finds one it did not load.
        <code className="inline">/import-claude</code> prepares an import of your Claude Code settings and MCP servers for you to review. <code className="inline">/plugin install</code> accepts Claude Code plugins containing skills, commands, agent profiles and MCP servers, and refuses plugins that need hooks or LSP servers.
        See <Link href={p("/docs/claude-code")} className="body-link">Move from Claude Code</Link>.
      </>
    ),
    sources: ["README.md", "docs/CLAUDE_PLUGIN_COMPAT.md"],
  },
  {
    q: "Which operating systems does Codewhale run on?",
    a: (
      <>
        Release binaries cover Linux, macOS and Windows on x64 and arm64; the Linux binaries are static (musl). Android arm64 / Termux is preview support.
        The OS command sandbox is Seatbelt on macOS and bubblewrap on Linux when it is installed. Windows has no OS sandbox yet, and Full Access turns the sandbox off on every platform.
        See the <Link href={p("/install")} className="body-link">install guide</Link> for each platform.
      </>
    ),
    sources: ["README.md", "docs/INSTALL.md", "docs/SANDBOX.md"],
  },
  {
    q: "Can I use Codewhale from VS Code?",
    a: (
      <>
        Yes. CodeWhale GUI (VS Code) is the community-maintained graphical frontend for the same engine: agent chat, threads, and file changes in a VS Code sidebar, over the local Runtime you already run. Install it from the <a href="https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode" className="body-link">VS Code Marketplace</a>; the source is on <a href="https://github.com/HengQuWorld/CodeWhale-VSCode" className="body-link">GitHub</a>.
      </>
    ),
    sources: ["README.md"],
  },
  {
    q: "What's the difference between codewhale and codewhale-tui?",
    a: (
      <>
        Since v0.9.5 there is one compiled runtime: the <code className="inline">codewhale</code> command contains the terminal UI directly — there is no separate TUI executable to install.
        Release installers also expose <code className="inline">codew</code> as a byte-identical short name, and <code className="inline">codewhale update</code> refreshes any legacy <code className="inline">codewhale-tui</code> command path from the same verified bytes.
        <code className="inline">codewhale-tui</code> survives as the internal TUI crate compiled into the <code className="inline">codewhale-cli</code> Cargo package, so Cargo users install just <code className="inline">codewhale-cli</code>.
      </>
    ),
    sources: ["README.md", "CHANGELOG.md"],
  },
  {
    q: "Is Codewhale the same as DeepSeek TUI? What about the rename?",
    a: (
      <>
        Yes. Codewhale is the new name for what was previously called DeepSeek TUI.
        The canonical command is now <code className="inline">codewhale</code>. Legacy <code className="inline">deepseek</code> and <code className="inline">deepseek-tui</code> commands remain as compatibility shims — they still work.
        Config lives at <code className="inline">~/.codewhale/</code>. Legacy <code className="inline">~/.deepseek/</code> config is still read as a compatibility fallback, and <code className="inline">DEEPSEEK_*</code> env vars continue to work.
        DeepSeek is not deprecated. The rename reflects a mission idea put in this version: Codewhale as an agentic terminal for open models across providers, not a narrowing away from DeepSeek.
      </>
    ),
    sources: ["docs/REBRAND.md", "README.md"],
  },
  {
    q: "How do I set my API key?",
    a: (
      <>
        <pre tabIndex={0} className="code-block mb-2">
{`# Method 1: Environment variable
export DEEPSEEK_API_KEY=sk-...

# Method 2: Saved key (recommended — survives shell restarts)
codewhale auth set --provider deepseek     # prompts for the key
# scripted: pipe it in with --api-key-stdin

# Check what's active:
codewhale auth status    # shows config, keyring, and env-var state
codewhale doctor         # offline configuration checks`}
        </pre>
        <p>
          Saved keys take precedence over environment variables. Avoid putting a key directly on the command line, where it lands in shell history.
          Use <code className="inline">codewhale auth clear --provider deepseek</code> to remove a saved key.
        </p>
        <p>
          To test a live connection, run <code className="inline">codewhale doctor --probe-api</code> for your hosted provider or <code className="inline">codewhale doctor --probe-local</code> for a local endpoint. The local probe may start a managed service such as Ollama.
        </p>
      </>
    ),
    sources: ["#907", "#1545", "docs/CONFIGURATION.md"],
  },
  {
    q: "Which providers does Codewhale support?",
    a: (
      <>
        <p className="mb-2">The current source declares {FACTS.providers.length} built-in provider routes:</p>
        <ul className="list-disc pl-5 space-y-1 text-sm text-ink-soft mb-3">
          <li><strong>DeepSeek</strong> — bundled default with a native API route, reasoning streaming, cache metrics, and thinking effort control.</li>
          <li><strong>OpenRouter</strong> — unified API for DeepSeek models and other open-model routes.</li>
          <li><strong>{FACTS.providers.length - 2} more routes</strong> — including OpenAI-compatible, Anthropic, Mistral AI, OpenAI Codex, xAI, Moonshot/Kimi, Z.ai, MiniMax, StepFun, Volcengine Ark, Baidu Qianfan, Model Studio, NVIDIA NIM, Fireworks AI, Together AI, DeepInfra, SiliconFlow, Novita AI, Hugging Face, Arcee AI, AtlasCloud, and the keyless local endpoints SGLang, vLLM, and Ollama. <Link href={p("/models")} className="body-link">The full list is generated from the provider registry</Link>.</li>
        </ul>
        <p>
          Set the corresponding env var (e.g. <code className="inline">OPENROUTER_API_KEY</code>) and your provider in <code className="inline">~/.codewhale/config.toml</code>.
          Self-hosted OpenAI-compatible endpoints are supported through the provider config.
        </p>
      </>
    ),
    sources: ["docs/CONFIGURATION.md", "#1978", "#1710"],
  },
  {
    q: "How do I use OpenRouter with Codewhale?",
    a: (
      <>
        <p className="mb-2">
          To make OpenRouter the default, put the provider and model settings before the provider table in <code className="inline">~/.codewhale/config.toml</code>:
        </p>
        <pre tabIndex={0} className="code-block mb-2">
{`# ~/.codewhale/config.toml
provider = "openrouter"
default_text_model = "deepseek/deepseek-v4.1-flash"

[providers.openrouter]
api_key = "sk-or-v1-..."`}
        </pre>
        <p>
          OpenRouter uses the same reasoning/cache parser as the native DeepSeek provider.
          Model IDs are OpenRouter's own slugs (e.g. <code className="inline">deepseek/deepseek-v4.1-flash</code>); pick the route with <code className="inline">--provider openrouter</code> or top-level <code className="inline">provider = &quot;openrouter&quot;</code>.
        </p>
      </>
    ),
    sources: ["docs/CONFIGURATION.md", "#1978"],
  },
  {
    q: "Can I use self-hosted or local models (vLLM, Ollama, llama.cpp)?",
    a: (
      <>
        Yes. Use the <code className="inline">vllm</code>, <code className="inline">sglang</code>, or <code className="inline">ollama</code> providers with your local endpoint.
        For OpenAI-compatible endpoints (llama.cpp server, text-generation-webui, Aphrodite, etc.), you can use the <code className="inline">openai</code> provider with a custom <code className="inline">base_url</code>.
        Codewhale also respects <code className="inline">DEEPSEEK_ALLOW_INSECURE_HTTP=true</code> for local HTTP endpoints.
        Hugging Face Inference Providers are also available through the <code className="inline">huggingface</code> provider. Broader Hub discovery, model cards, datasets, and Jobs belong to Model Lab.
      </>
    ),
    sources: ["#574", "#1303", "docs/CONFIGURATION.md"],
  },
  {
    q: "What are Plan, Work, and Operate modes?",
    a: (
      <>
        <ul className="list-disc pl-5 space-y-2 text-sm text-ink-soft">
          <li><strong>Plan</strong> — Read-only investigation. Can grep, read files, list directories, fetch URLs. Cannot write or execute shell.</li>
          <li><strong>Work</strong> — Interactive work with files, commands and connected tools. Tool availability and approval prompts follow the active configuration and permission posture.</li>
          <li><strong>Operate</strong> — Direct tools follow the same permission, sandbox, shell, and safety rules as Work. Fleet workers are preferred for independent, parallel, background, or long-running work, but delegation is not mandatory. Workflow is optional for ordered phases and gates.</li>
        </ul>
        <p className="mt-2">
          When the composer is empty, press <kbd className="font-mono text-xs px-1.5 py-0.5 hairline-t hairline-b hairline-l hairline-r">Tab</kbd> to cycle modes.
          Press <kbd className="font-mono text-xs px-1.5 py-0.5 hairline-t hairline-b hairline-l hairline-r">Shift+Tab</kbd> to cycle the independent Ask / Auto-Review / Full Access permission posture; Plan remains read-only.
        </p>
      </>
    ),
    sources: ["docs/MODES.md"],
  },
  {
    q: pickText(AUTO_ROUTING_COPY.question, "en"),
    a: pickText(AUTO_ROUTING_COPY.answer, "en"),
    sources: ["docs/CONFIGURATION.md"],
  },
  {
    q: "What does /goal do?",
    a: (
      <>
        <code className="inline">/goal</code> sets a goal for the current TUI session.
        App-server clients can also persist a thread-scoped goal through the
        <code className="inline">thread/goal/*</code> methods. It does not add another
        app mode; the mode switcher remains Plan, Work, and Operate, while permission posture is selected independently.
        Track progress in <a href="https://github.com/codewhale-hq/CodeWhale/issues/891" className="body-link">#891</a>.
      </>
    ),
    sources: ["#891"],
  },
  {
    q: "Is my code safe? What sandboxing does Codewhale use?",
    a: (
      <>
        The Codewhale runtime, workspace state, and audit log stay on your machine.
        Codewhale counts anonymous usage by default and says so at first launch.
        Turning it off is a saved choice that later versions keep; showing the notice
        never records any acceptance on your behalf. While on, a
        session posts aggregate session, feature, and error counts
        and closed enums to a first-party endpoint at telemetry.codewhale.net,
        a Cloudflare Worker whose full source is in the repository. Its storage has no IP,
        country, or geo column, records no request logs,
        and retains records for three months. Optional PostHog forwarding requires
        separate operator configuration and verified IP-safe egress; its retention
        is a separate project setting. Source support does not establish activation. Set{" "}
        <code className="inline">telemetry_endpoint = &quot;&quot;</code> to stay
        enabled and contact nobody. It never carries conversations, code, prompts,
        files, file/repo/branch names, model content, credentials, or a per-turn or
        per-tool timeline (see the{" "}
        <a href="https://github.com/codewhale-hq/CodeWhale/blob/main/docs/TELEMETRY.md" className="body-link">telemetry schema</a>;
        off with <code className="inline">codewhale config set telemetry false</code>
        or <code className="inline">CODEWHALE_TELEMETRY=0</code>). There is no
        mandatory hosted relay. The hosted
        provider you select receives the prompt, project context, tool definitions,
        and tool results required for that turn. Use a loopback local-model route to
        keep model inference local.
        OS command sandboxing is platform-specific: Codewhale uses <strong>Seatbelt</strong> on macOS when available. On Linux it uses <strong>bubblewrap</strong> by default whenever <code className="inline">/usr/bin/bwrap</code> is installed and a probe shows it works; <code className="inline">prefer_bwrap = false</code> opts out. Otherwise commands have no Codewhale OS wrapper. Windows currently reports no OS sandbox.
        Workspace boundaries default to <code className="inline">--workspace</code>. <code className="inline">/trust</code> lifts them.
        Permission posture is configurable per session.
      </>
    ),
    sources: [".github/SECURITY.md", "docs/PROVIDERS.md", "docs/RUNTIME_API.md"],
  },
  {
    q: "How do MCP servers work?",
    a: (
      <>
        Codewhale is a bidirectional MCP client and server. Define servers in <code className="inline">~/.codewhale/mcp.json</code>.
        Tools appear as <code className="inline">mcp_&lt;server&gt;_&lt;tool&gt;</code>. You can also expose Codewhale as an MCP server with <code className="inline">codewhale serve --mcp</code>.
        See the <Link href={p("/docs/mcp")} className="body-link">docs page</Link> for configuration examples.
      </>
    ),
    sources: ["docs/MCP.md"],
  },
  {
    q: "How do I contribute?",
    a: (
      <>
        No CLA required. Fork, branch with conventional commits (<code className="inline">feat:</code>, <code className="inline">fix:</code>, etc.), run the local checks, open a PR.
        The maintainer reads everything personally. Start with issues labeled <code className="inline">good first issue</code>.
        See the <Link href={p("/contribute")} className="body-link">contribute page</Link> and <a href="https://github.com/codewhale-hq/CodeWhale/blob/main/CONTRIBUTING.md" className="body-link">CONTRIBUTING.md</a>.
      </>
    ),
    sources: ["CONTRIBUTING.md"],
  },
  {
    q: "I'm in China — how do I install? Downloads are slow.",
    a: (
      <>
        Use mirror registries:
        <pre tabIndex={0} className="code-block my-2">
{`# npm mirror
npm config set registry https://registry.npmmirror.com
npm install -g codewhale

# Cargo mirror (Tsinghua TUNA)
# Add to ~/.cargo/config.toml:
[source.crates-io]
replace-with = "tuna"
[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"`}
        </pre>
        <p>
          Prebuilt binaries are also available from <a href="https://github.com/codewhale-hq/CodeWhale/releases" className="body-link">GitHub Releases</a>.
          A maintained CNB mirror covers its documented targets; no Gitee mirror is advertised until one exists.
        </p>
      </>
    ),
    sources: ["README.md", "#1914", "docs/CNB_MIRROR.md"],
  },
  {
    q: "Is codewhale.net the official site? What about mirrors?",
    a: (
      <>
        <p className="mb-2">
          <strong>codewhale.net</strong> and <strong>www.codewhale.net</strong> are the
          official Codewhale sites, deployed on Cloudflare. The website source is open
          and lives under <code className="inline">web/</code> in the{" "}
          <code className="inline">codewhale-hq/CodeWhale</code> repository — anyone can
          self-deploy it as a mirror.
        </p>
        <p className="mb-2">
          All official releases and SHA-256 checksums are distributed exclusively through{" "}
          <a href="https://github.com/codewhale-hq/CodeWhale/releases" className="body-link">GitHub Releases</a>.
          The npm package downloads verified binaries from GitHub Releases.
        </p>
        <p className="mb-2">
          A CNB mirror is maintained for users who cannot reliably reach GitHub
          (<Link href={p("/install")} className="body-link">docs/CNB_MIRROR.md</Link>).
          Cargo users can use the TUNA mirror for faster downloads in China.
        </p>
        <p>
          Self-deployed website copies, mirror sites, and third-party packages are not
          controlled by the Codewhale project. Verify download sources and checksums.
        </p>
      </>
    ),
    sources: ["#2624", "#3421", "docs/CNB_MIRROR.md"],
  },
  {
    q: "My API key was rejected or I get auth errors on first run.",
    a: (
      <>
        <p className="mb-2">Start with <code className="inline">codewhale doctor</code> for an offline report of configuration paths, declared credential sources and tool setup. It does not test live connectivity by default.</p>
        <p className="mb-2">Use <code className="inline">codewhale doctor --probe-api</code> to explicitly test your hosted provider, or <code className="inline">codewhale doctor --probe-local</code> for a local endpoint. A local probe may start a managed service such as Ollama.</p>
        <p className="mb-2">Common causes:</p>
        <ul className="list-disc pl-5 space-y-1 text-sm text-ink-soft">
          <li>Stale <code className="inline">DEEPSEEK_API_KEY</code> in shell startup file — open a fresh shell or use <code className="inline">codewhale auth set</code></li>
          <li>Key from wrong provider — make sure the key matches the provider you're using</li>
          <li>Network connectivity — check <code className="inline">curl https://api.deepseek.com/v1/models</code></li>
        </ul>
      </>
    ),
    sources: ["#907", "#1545", "docs/CONFIGURATION.md"],
  },
  {
    q: "What is Model Lab? What Hugging Face pieces are available?",
    a: (
      <>
        The <code className="inline">huggingface</code> provider is the shipped OpenAI-compatible route for Hugging Face Inference Providers.
        Model Lab is the planned open-model infrastructure layer for Hub discovery, model cards, datasets, safetensors adapters, and Jobs.
        Track broader progress in <a href="https://github.com/codewhale-hq/CodeWhale/issues/1977" className="body-link">#1977</a>.
      </>
    ),
    sources: ["#1977", "docs/MODEL_LAB.md"],
  },
  {
    q: "Why is token consumption so high? / Why is cache hit rate low?",
    a: pickText(AUTO_ROUTING_COPY.cache, "en"),
    sources: ["docs/CONFIGURATION.md"],
  },
  {
    q: "How do I update Codewhale?",
    a: (
      <>
        <pre tabIndex={0} className="code-block mb-2">
{`# Installer or release-binary installs
codewhale update

# npm (codewhale update refuses npm installs)
npm install -g codewhale@latest

# Cargo
cargo install codewhale-cli --locked --force

# Homebrew tap
brew update && brew upgrade codewhale`}
        </pre>
        <p>
          If you installed with npm or Homebrew, update through that package manager; <code className="inline">codewhale update</code> leaves package-managed installs unchanged.
          If a mirror is lagging, download directly from <a href="https://github.com/codewhale-hq/CodeWhale/releases" className="body-link">GitHub Releases</a>.
        </p>
      </>
    ),
    sources: ["README.md", "#1869", "#1914"],
  },
];

export const faqZh = (p: SitePath): FaqItem[] => [
  {
    q: "Codewhale 是什么？",
    a: (
      <>
        Codewhale 是一个开源智能体，用于构建应用和工具、自动化工作流，并在文件、终端命令和已连接的应用之间完成任务。通过插件和 MCP 服务添加工具，或在自己的脚本中使用 API。每项服务都需要单独配置并完成身份验证。使用你已有的模型 API，或连接本地、自托管推理服务。运行 <code className="inline">codewhale</code> 启动，并设定访问范围和审批方式。DeepSeek 是内置默认路由，模型与提供商仍由你选择。
      </>
    ),
    sources: ["README.md", "docs/GUIDE.md", "docs/PLUGINS.md", "docs/MCP.md", "docs/PROVIDERS.md"],
  },
  {
    q: "如何安装 Codewhale？",
    a: (
      <>
        <p className="mb-2">已发布渠道的更新时间与平台覆盖各不相同：</p>
        <pre tabIndex={0} className="code-block mb-2">
{`# GitHub Releases 二进制（macOS / Linux 推荐方式）
${GETTING_STARTED_STEPS[0].commands[0]}

# npm 其他方式 — 无需 Rust 工具链
npm install -g codewhale

# Cargo（需要 Rust 1.89+；安装 codewhale 命令）
cargo install codewhale-cli --locked

# Linux 上的 Homebrew（tap；已在 Ubuntu 上测试）
brew install Hmbown/deepseek-tui/codewhale

# 直接下载
# https://github.com/codewhale-hq/CodeWhale/releases`}
        </pre>
        <p>
          输入 <code className="inline">codewhale</code> 即可启动。首次运行会自动创建 <code className="inline">~/.codewhale/</code>。旧版 <code className="inline">~/.deepseek/</code> 仍会作为兼容回退读取。
          Android arm64 / Termux 仍是预览支持：只有当所选 npm 包版本对应的 GitHub Release 发布了匹配的 Android 资产时，npm 安装才可用。
          查看 <Link href={p("/install")} className="body-link">完整安装指南</Link> 了解国内镜像、Docker 和故障排除。
        </p>
      </>
    ),
    sources: ["README.md", "docs/INSTALL.md", "#1860", "#1914"],
  },
  {
    q: "Codewhale 免费吗？需要注册账号吗？",
    a: (
      <>
        Codewhale 免费，采用 MIT 许可证，终端应用无需账号。使用你自己的 API 密钥时，费用直接付给提供商；通过 Ollama、vLLM 或 SGLang 运行的本地模型无需密钥。
        通过 <code className="inline">/provider</code> 添加的密钥只保存在你的机器上。价格已知时，页脚和 <code className="inline">/cost</code> 会显示本次会话的花费；价格未知时显示为未知，绝不显示为免费。
      </>
    ),
    sources: ["README.md", "LICENSE"],
  },
  {
    q: "我在用 Claude Code、Cursor 或 Copilot，哪些设置可以沿用？",
    a: (
      <>
        Codewhale 会自动读取 <code className="inline">AGENTS.md</code>。为其他工具编写的指令文件，例如 <code className="inline">CLAUDE.md</code>、<code className="inline">.cursorrules</code> 或 <code className="inline">.github/copilot-instructions.md</code>，只有在你通过 <code className="inline">project_instruction_imports = [&quot;claude&quot;]</code>（或 <code className="inline">&quot;cursor&quot;</code>、<code className="inline">&quot;cline&quot;</code>、<code className="inline">&quot;windsurf&quot;</code>、<code className="inline">&quot;copilot&quot;</code>、<code className="inline">&quot;all&quot;</code>）选择启用时才会加载；发现未加载的文件时，Codewhale 会给出提示。
        <code className="inline">/import-claude</code> 会准备导入你的 Claude Code 设置和 MCP 服务器，供你审阅。<code className="inline">/plugin install</code> 接受包含技能、命令、智能体配置和 MCP 服务器的 Claude Code 插件，需要 hooks 或 LSP 服务器的插件会被拒绝。
        详见<Link href={p("/docs/claude-code")} className="body-link">从 Claude Code 迁移</Link>。
      </>
    ),
    sources: ["README.md", "docs/CLAUDE_PLUGIN_COMPAT.md"],
  },
  {
    q: "Codewhale 支持哪些操作系统？",
    a: (
      <>
        发布的二进制文件覆盖 x64 和 arm64 上的 Linux、macOS 和 Windows；Linux 二进制为静态链接（musl）。Android arm64 / Termux 为预览支持。
        命令的系统沙箱在 macOS 上使用 Seatbelt，在 Linux 上安装了 bubblewrap 时使用 bubblewrap。Windows 目前没有系统沙箱，Full Access 在所有平台上都会关闭沙箱。
        各平台的安装方式见<Link href={p("/install")} className="body-link">安装指南</Link>。
      </>
    ),
    sources: ["README.md", "docs/INSTALL.md", "docs/SANDBOX.md"],
  },
  {
    q: "可以在 VS Code 中使用 Codewhale 吗？",
    a: (
      <>
        可以。CodeWhale GUI（VS Code）是社区维护的图形前端，运行同一个引擎：在 VS Code 侧边栏中对话、管理线程并查看文件变更，连接你本机已在运行的 Runtime。可从 <a href="https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode" className="body-link">VS Code Marketplace</a> 安装；源码见 <a href="https://github.com/HengQuWorld/CodeWhale-VSCode" className="body-link">GitHub</a>。
      </>
    ),
    sources: ["README.md"],
  },
  {
    q: "codewhale 和 codewhale-tui 有什么区别？",
    a: (
      <>
        自 v0.9.5 起只有一个编译好的运行时：<code className="inline">codewhale</code> 命令直接内置终端 UI——不再有需要单独安装的 TUI 可执行文件。
        发布安装器同时提供字节完全相同的 <code className="inline">codew</code> 短名称，<code className="inline">codewhale update</code> 会用同一份经过校验的字节刷新任何遗留的 <code className="inline">codewhale-tui</code> 命令路径。
        <code className="inline">codewhale-tui</code> 仅以内部 TUI crate 的形式存在，编译进 <code className="inline">codewhale-cli</code> Cargo 包，因此 Cargo 用户只需安装 <code className="inline">codewhale-cli</code>。
      </>
    ),
    sources: ["README.md", "CHANGELOG.md"],
  },
  {
    q: "Codewhale 和 DeepSeek TUI 是什么关系？改名是怎么回事？",
    a: (
      <>
        Codewhale 是 DeepSeek TUI 的新名称。当前的主命令是 <code className="inline">codewhale</code>。旧的 <code className="inline">deepseek</code> 和 <code className="inline">deepseek-tui</code> 命令作为兼容垫片继续有效。
        配置存放在 <code className="inline">~/.codewhale/</code>。旧版 <code className="inline">~/.deepseek/</code> 配置仍会作为兼容回退读取，<code className="inline">DEEPSEEK_*</code> 环境变量继续有效。
        DeepSeek 并未被弃用。改名是为了体现 Codewhale 更广泛的使命——成为面向所有提供商的开放模型智能体终端，而非弱化 DeepSeek 的地位。
      </>
    ),
    sources: ["docs/REBRAND.md", "README.md"],
  },
  {
    q: "如何设置 API 密钥？",
    a: (
      <>
        <pre tabIndex={0} className="code-block mb-2">
{`# 方法 1：环境变量
export DEEPSEEK_API_KEY=sk-...

# 方法 2：保存密钥（推荐 — 重启 Shell 后仍然有效）
codewhale auth set --provider deepseek     # 会提示输入密钥
# 脚本中：用 --api-key-stdin 通过管道传入

# 查看当前状态：
codewhale auth status    # 显示配置、密钥环和环境变量状态
codewhale doctor         # 离线配置检查`}
        </pre>
        <p>
          已保存的密钥优先于环境变量。不要把密钥直接写在命令行里，否则会留在 Shell 历史中。
          使用 <code className="inline">codewhale auth clear --provider deepseek</code> 移除已保存的密钥。
        </p>
        <p>
          要实际测试连接，运行 <code className="inline">codewhale doctor --probe-api</code> 检查托管提供商，或用 <code className="inline">codewhale doctor --probe-local</code> 检查本地端点。本地探针可能启动由桌面应用管理的 Ollama 等服务。
        </p>
      </>
    ),
    sources: ["#907", "#1545", "docs/CONFIGURATION.md"],
  },
  {
    q: "Codewhale 支持哪些提供商？",
    a: (
      <>
        <p className="mb-2">当前源码声明了 {FACTS.providers.length} 条内置提供商路由：</p>
        <ul className="list-disc pl-5 space-y-1 text-sm text-ink-soft mb-3">
          <li><strong>DeepSeek</strong> — 内置默认原生 API 路由，支持推理流、缓存指标和思考力度控制。</li>
          <li><strong>OpenRouter</strong> — 统一 API，可访问 DeepSeek 和其他开放模型路由。</li>
          <li><strong>另外 {FACTS.providers.length - 2} 条路由</strong>——包括 OpenAI 兼容、Anthropic、Mistral AI、OpenAI Codex、xAI、Moonshot/Kimi、Z.ai、MiniMax、StepFun、Volcengine Ark、百度千帆、Model Studio、NVIDIA NIM、Fireworks、Together AI、DeepInfra、SiliconFlow、Novita、Hugging Face、Arcee AI、AtlasCloud，以及无需密钥的本地端点 SGLang、vLLM 和 Ollama。<Link href={p("/models")} className="body-link">完整列表由提供商注册表生成</Link>。</li>
        </ul>
        <p>
          设置对应的环境变量（如 <code className="inline">OPENROUTER_API_KEY</code>）并在 <code className="inline">~/.codewhale/config.toml</code> 中配置你的提供商。
          自托管 OpenAI 兼容端点可通过 provider 配置接入。
        </p>
      </>
    ),
    sources: ["docs/CONFIGURATION.md", "#1978", "#1710"],
  },
  {
    q: "如何使用 OpenRouter？",
    a: (
      <>
        <p className="mb-2">
          要将 OpenRouter 设为默认路由，请在 <code className="inline">~/.codewhale/config.toml</code> 中把提供商和模型设置放在提供商配置表之前：
        </p>
        <pre tabIndex={0} className="code-block mb-2">
{`# ~/.codewhale/config.toml
provider = "openrouter"
default_text_model = "deepseek/deepseek-v4.1-flash"

[providers.openrouter]
api_key = "sk-or-v1-..."`}
        </pre>
        <p>
          OpenRouter 使用与原生 DeepSeek 提供商相同的推理/缓存解析器。
          模型 ID 使用 OpenRouter 自己的 slug（如 <code className="inline">deepseek/deepseek-v4.1-flash</code>）；通过 <code className="inline">--provider openrouter</code> 或顶层 <code className="inline">provider = &quot;openrouter&quot;</code> 选择路由。
        </p>
      </>
    ),
    sources: ["docs/CONFIGURATION.md", "#1978"],
  },
  {
    q: "可以使用自托管或本地模型吗（vLLM、Ollama、llama.cpp）？",
    a: (
      <>
        可以。使用 <code className="inline">vllm</code>、<code className="inline">sglang</code> 或 <code className="inline">ollama</code> 提供商连接本地端点。
        对于 OpenAI 兼容端点（llama.cpp server、text-generation-webui 等），可以使用 <code className="inline">openai</code> 提供商并设置自定义 <code className="inline">base_url</code>。
        Codewhale 也支持 <code className="inline">DEEPSEEK_ALLOW_INSECURE_HTTP=true</code> 用于本地 HTTP 端点。
        Hugging Face Inference Providers 也可以通过 <code className="inline">huggingface</code> provider 使用。更完整的 Hub 发现、模型卡片、数据集和 Jobs 属于 Model Lab。
      </>
    ),
    sources: ["#574", "#1303", "docs/CONFIGURATION.md"],
  },
  {
    q: "Plan、Work、Operate 三种模式有什么区别？",
    a: (
      <>
        <ul className="list-disc pl-5 space-y-2 text-sm text-ink-soft">
          <li><strong>Plan（计划）</strong> — 只读调查。可以 grep、读文件、列目录、抓取 URL。不能写入或执行 Shell。</li>
          <li><strong>Work（执行）</strong> — 交互式执行任务，可处理文件、运行命令并使用已连接的工具。工具是否可用以及何时请求批准，取决于当前配置和权限姿态。</li>
          <li><strong>Operate（编排）</strong> — 直接工具遵循与 Work 相同的权限、沙箱、Shell 和安全规则。独立、并行、后台或长时间工作会优先交给 fleet worker，但不强制委派；只有需要有序阶段和门禁时才需要 Workflow。</li>
        </ul>
        <p className="mt-2">
          输入区为空时，按 <kbd className="font-mono text-xs px-1.5 py-0.5 hairline-t hairline-b hairline-l hairline-r">Tab</kbd> 切换模式。
          按 <kbd className="font-mono text-xs px-1.5 py-0.5 hairline-t hairline-b hairline-l hairline-r">Shift+Tab</kbd> 循环独立的 Ask / Auto-Review / Full Access 权限姿态；Plan 始终只读。
        </p>
      </>
    ),
    sources: ["docs/MODES.md"],
  },
  {
    q: pickText(AUTO_ROUTING_COPY.question, "zh"),
    a: pickText(AUTO_ROUTING_COPY.answer, "zh"),
    sources: ["docs/CONFIGURATION.md"],
  },
  {
    q: "什么是 Goal 模式？现在可用吗？",
    a: (
      <>
        <code className="inline">/goal</code> 为当前 TUI 会话设置目标，支持 <code className="inline">pause</code>、<code className="inline">resume</code>、<code className="inline">complete</code>、<code className="inline">blocked</code> 和 <code className="inline">clear</code> 控制。
        App-server 客户端也可以通过 <code className="inline">thread/goal/*</code> 方法持久化线程范围的目标，支持 <code className="inline">set</code>、<code className="inline">get</code> 和 <code className="inline">clear</code>。
        它不会新增一个应用模式；模式切换器仍然是 Plan、Work 和 Operate，权限姿态独立选择。
        跟踪进展：<a href="https://github.com/codewhale-hq/CodeWhale/issues/891" className="body-link">#891</a>。
      </>
    ),
    sources: ["#891"],
  },
  {
    q: "我的代码安全吗？Codewhale 使用什么沙箱机制？",
    a: (
      <>
        Codewhale 运行时、工作区状态与审计日志保留在你的机器上。Codewhale 默认统计匿名使用量，并在首次启动时告知你。
        关闭是会被后续版本保留的选择；显示告知绝不会代你记录任何同意。开启时，会话只会把聚合的会话、功能与错误计数以及封闭枚举 POST 到 telemetry.codewhale.net 上的第一方端点，
        那是一个 Cloudflare Worker，完整源码就在仓库里。
        它的存储中没有 IP、国家或地理位置列，不记录请求日志，保留期固定为三个月。
        可选的 PostHog 转发需要运营方单独配置，并验证出口不会转发客户端 IP；其保留期由项目另行设置。源码支持不代表已启用。
        若想保持启用但不联系任何服务器，设置 <code className="inline">telemetry_endpoint = &quot;&quot;</code>。
        它永远不会携带对话、代码、prompt、文件、文件/仓库/分支名、模型内容、凭据，也不发送逐轮或逐工具时间线（见<a href="https://github.com/codewhale-hq/CodeWhale/blob/main/docs/TELEMETRY.md" className="body-link">遥测 schema</a>；
        可用 <code className="inline">codewhale config set telemetry false</code> 或
        <code className="inline">CODEWHALE_TELEMETRY=0</code> 关闭）。也不要求经过托管中继。你选择的托管 provider 会收到本轮所需的
        prompt、项目上下文、工具定义与工具结果。若要让模型推理也保持本地，请使用回环地址上的本地模型路由。
        OS 命令沙箱因平台而异：macOS 在可用时使用 <strong>Seatbelt</strong>。Linux 默认在 <code className="inline">/usr/bin/bwrap</code> 已安装且探测可用时使用 <strong>bubblewrap</strong>；<code className="inline">prefer_bwrap = false</code> 退出。否则命令没有 Codewhale OS 包装器。Windows 当前报告无 OS 沙箱。
        工作区边界默认为 <code className="inline">--workspace</code>。<code className="inline">/trust</code> 可解除边界。
        权限姿态可按会话配置。
      </>
    ),
    sources: [".github/SECURITY.md", "docs/PROVIDERS.md", "docs/RUNTIME_API.md"],
  },
  {
    q: "MCP 服务器如何工作？",
    a: (
      <>
        Codewhale 是双向 MCP 客户端和服务器。在 <code className="inline">~/.codewhale/mcp.json</code> 中定义服务器。
        工具以 <code className="inline">mcp_&lt;server&gt;_&lt;tool&gt;</code> 形式呈现。你也可以通过 <code className="inline">codewhale serve --mcp</code> 将 Codewhale 暴露为 MCP 服务器。
        查看 <Link href={p("/docs/mcp")} className="body-link">文档页面</Link> 了解配置示例。
      </>
    ),
    sources: ["docs/MCP.md"],
  },
  {
    q: "如何参与贡献？",
    a: (
      <>
        无需签署 CLA。Fork、用约定式提交（<code className="inline">feat:</code>、<code className="inline">fix:</code> 等）创建分支、通过本地检查、提交 PR。
        维护者亲自阅读每一条内容。从标记为 <code className="inline">good first issue</code> 的议题开始。
        查看 <Link href={p("/contribute")} className="body-link">贡献页面</Link> 和 <a href="https://github.com/codewhale-hq/CodeWhale/blob/main/CONTRIBUTING.md" className="body-link">CONTRIBUTING.md</a>。
      </>
    ),
    sources: ["CONTRIBUTING.md"],
  },
  {
    q: "我在国内，安装很慢怎么办？",
    a: (
      <>
        使用镜像源：
        <pre tabIndex={0} className="code-block my-2">
{`# npm 镜像
npm config set registry https://registry.npmmirror.com
npm install -g codewhale

# Cargo 镜像（清华 TUNA）
# 在 ~/.cargo/config.toml 中添加：
[source.crates-io]
replace-with = "tuna"
[source.tuna]
registry = "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/"`}
        </pre>
        <p>
          也可以从 <a href="https://github.com/codewhale-hq/CodeWhale/releases" className="body-link">GitHub Releases</a> 直接下载预编译二进制。
          维护中的 CNB 镜像覆盖其文档列出的目标；Gitee 镜像只有实际存在后才会对外展示。
        </p>
      </>
    ),
    sources: ["README.md", "#1914", "docs/CNB_MIRROR.md"],
  },
  {
    q: "codewhale.net 是官方网站吗？镜像站点呢？",
    a: (
      <>
        <p className="mb-2">
          <strong>codewhale.net</strong> 和 <strong>www.codewhale.net</strong> 是
          Codewhale 的官方站点，部署在 Cloudflare 上。网站源码存放于{" "}
          <code className="inline">codewhale-hq/CodeWhale</code> 仓库的{" "}
          <code className="inline">web/</code> 目录下，任何人都可自行部署为镜像。
        </p>
        <p className="mb-2">
          所有正式发布和 SHA-256 校验文件仅通过{" "}
          <a href="https://github.com/codewhale-hq/CodeWhale/releases" className="body-link">GitHub Releases</a> 分发。
          npm 包从 GitHub Releases 下载经校验的二进制。
        </p>
        <p className="mb-2">
          面向无法稳定访问 GitHub 的用户，提供 CNB 镜像（
          <Link href={p("/install")} className="body-link">docs/CNB_MIRROR.md</Link>）。
          Cargo 用户可使用 TUNA 镜像在国内加速下载。
        </p>
        <p>
          自行部署的网站副本、镜像站和第三方包不受 Codewhale 项目控制。
          请验证下载来源和校验和。
        </p>
      </>
    ),
    sources: ["#2624", "#3421", "docs/CNB_MIRROR.md"],
  },
  {
    q: "首次运行时提示 API 密钥被拒绝或认证错误？",
    a: (
      <>
        <p className="mb-2">先运行 <code className="inline">codewhale doctor</code>，查看配置路径、已声明的凭据来源和工具设置的离线报告。默认情况下，它不测试实际连接。</p>
        <p className="mb-2">用 <code className="inline">codewhale doctor --probe-api</code> 明确测试托管提供商，或用 <code className="inline">codewhale doctor --probe-local</code> 检查本地端点。本地探针可能启动由桌面应用管理的 Ollama 等服务。</p>
        <p className="mb-2">常见原因：</p>
        <ul className="list-disc pl-5 space-y-1 text-sm text-ink-soft">
          <li>Shell 启动文件中的 <code className="inline">DEEPSEEK_API_KEY</code> 已过期——打开新 Shell 或使用 <code className="inline">codewhale auth set</code></li>
          <li>密钥来自错误的提供商——确保密钥与你使用的提供商匹配</li>
          <li>网络连接问题——检查 <code className="inline">curl https://api.deepseek.com/v1/models</code></li>
        </ul>
      </>
    ),
    sources: ["#907", "#1545", "docs/CONFIGURATION.md"],
  },
  {
    q: "Model Lab 是什么？Hugging Face 哪些部分可用？",
    a: (
      <>
        <code className="inline">huggingface</code> provider 是已经接入的 OpenAI 兼容 Hugging Face Inference Providers 路由。
        Model Lab 是规划中的开放模型基础设施层：Hub 发现、模型卡片、数据集、safetensors 适配器和 Jobs。
        更完整的进展见 <a href="https://github.com/codewhale-hq/CodeWhale/issues/1977" className="body-link">#1977</a>。
      </>
    ),
    sources: ["#1977", "docs/MODEL_LAB.md"],
  },
  {
    q: "为什么 token 消耗这么大？/ 缓存命中率为什么低？",
    a: pickText(AUTO_ROUTING_COPY.cache, "zh"),
    sources: ["docs/CONFIGURATION.md"],
  },
  {
    q: "如何更新 Codewhale？",
    a: (
      <>
        <pre tabIndex={0} className="code-block mb-2">
{`# 安装器或发布二进制安装
codewhale update

# npm（codewhale update 不会更新 npm 安装）
npm install -g codewhale@latest

# Cargo
cargo install codewhale-cli --locked --force

# Homebrew tap
brew update && brew upgrade codewhale`}
        </pre>
        <p>
          如果通过 npm 或 Homebrew 安装，请用对应的包管理器更新；<code className="inline">codewhale update</code> 不会改动包管理器安装的版本。
          如果镜像延迟，请从 <a href="https://github.com/codewhale-hq/CodeWhale/releases" className="body-link">GitHub Releases</a> 直接下载。
        </p>
      </>
    ),
    sources: ["README.md", "#1869", "#1914"],
  },
];

