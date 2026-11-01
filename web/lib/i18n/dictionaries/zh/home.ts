import type { HomeDict } from "../types";

/** Simplified Chinese home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale：开源编程智能体",
  metaDescription: "Codewhale 是终端里的开源编程智能体。用你选择的托管模型或本地模型读取、修改和测试项目。",
  heroTitle: "适用于任意模型的开源编程智能体",
  heroIntro: "{brand} 在终端里读取你的项目、编辑文件并运行测试。连接托管模型或本地模型，并选择哪些操作需要你批准。",
  getCodewhale: "安装 Codewhale",
  heroInstallAria: "安装命令",
  exploreProduct: "探索 Codewhale",
  shotPreview: "终端预览",
  screenshotAlt:
    "Codewhale v{version} 的终端实录画面，包含对话、消息输入框与会话控制。",
  latestRelease: "最新发布 {tag}",
  releaseUnavailable: "发布状态暂不可用",
  proofContributors: "{count} 位贡献者",
  currentSource: "源码",
  sourceCandidate: "未发布",
  publishedRelease: "已发布",
  gainHeading: "在终端里构建与自动化",
  gainLede: "描述你想构建或自动完成的工作。Codewhale 可以编辑文件、运行命令并检查结果，访问权限由你控制。",
  gain: [
    [
      "构建应用和工具",
      "创建应用、添加功能或编写脚本。Codewhale 可以处理项目文件、运行代码并测试构建结果。"
    ],
    [
      "让重复工作自动完成",
      "从终端、脚本或 CI 中运行工作流。任务较大时，可以把部分工作交给使用不同模型的 Fleet 智能体团队。"
    ],
    [
      "连接你常用的工具",
      "通过插件和 MCP 服务器添加工具，或在自己的脚本中调用 API。每项服务都需要单独配置和身份验证。"
    ]
  ],
  chapterModels: "你的模型",
  modelsHeading: "使用你选择的模型",
  modelsBody:
    "连接你的提供商账户、OpenAI 兼容端点，或本地及自托管模型。为会话选择模型，也可以为 Fleet 中的每个智能体分别选择模型。",
  modelsFacts: [
    [
      "你的 API 账户",
      "使用自己的密钥连接 DeepSeek、OpenAI、Anthropic、Google 等提供商。默认模型 deepseek-flash 现已由 DeepSeek-V4.1-Flash 提供。"
    ],
    [
      "你的网关",
      "连接 OpenAI 兼容端点，选择它提供的模型。"
    ],
    [
      "你的推理服务",
      "通过 Ollama、vLLM 或 SGLang 运行本地或自托管模型。"
    ],
    [
      "你的 ChatGPT 套餐",
      "运行 codewhale auth chatgpt 登录，即可用 ChatGPT 套餐代替 API 密钥。实验功能。"
    ]
  ],
  modelsLink: "浏览模型与提供商",
  startHeading: "开始使用",
  startLede: "安装 Codewhale，连接模型，然后打开项目文件夹。需要时，再添加插件或更多智能体。",
  startGuideLink: "按照新手指引操作",
  startVocabularyLink: "查名词",
  chapterAvailability: "在哪里运行",
  availabilityHeading: "当前可用与开发中的功能",
  availabilityLede: "终端和本地浏览器客户端现已可用。原生桌面应用与重建中的托管网页应用正在开发。",
  availability: [
    [
      "终端与本地浏览器",
      "已发布",
      "在 Linux、macOS 或 Windows 上安装，然后运行 codewhale，或运行 codewhale web 打开本地浏览器客户端。也可以用 npm 或 Cargo 安装；Android 上的 Termux 版本为预览版。"
    ],
    [
      "CodeWhale GUI（VS Code）",
      "可用",
      "由社区维护的独立项目：在 VS Code 侧边栏中连接同一个 Codewhale Runtime，进行对话、管理线程并查看文件变更。可从 VS Code Marketplace 安装。",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "托管网页应用",
      "开发中",
      "正在重建，以与桌面应用保持一致，暂未开放。如需现在在浏览器中使用，请在本机运行 codewhale web。"
    ],
    [
      "桌面端",
      "开发版本",
      "基于同一引擎的原生应用。审阅并提交改动、跨项目跟进工作，还能编辑一条消息并作为新分支重新发送。暂无公开下载。"
    ]
  ],
  availabilityNote: "终端、本地浏览器和 GUI 无需 Codewhale 账户。使用你自己的提供商密钥时，相应用量由该提供商计费。",
  surfacesHeading: "使用文件和工具开展工作",
  surfaces: [
    [
      "文件与终端",
      "创建文件、运行命令、分析数据，并测试你构建的成果。工作目录和权限由你设定。"
    ],
    [
      "插件与已连接的应用",
      "通过插件和 MCP 添加技能与工具。审核并启用你希望智能体使用的连接。"
    ],
    [
      "浏览器与电脑操作 · 预览",
      "使用浏览器工具和 Computer Use 插件，在你授予的访问范围内操作应用与网站。"
    ],
    [
      "随时接着做的会话",
      "将对话、工具结果和工作记录保存在一起。在终端或本地浏览器客户端中继续之前的任务。"
    ],
    [
      "智能体团队",
      "用 Fleet 将大任务分配给不同角色和模型的智能体，在一处查看它们的进度。"
    ]
  ],
  runtimeLink: "探索工具与集成",
  installBandHeading: "在 macOS 或 Linux 上安装",
  copy: "复制",
  copied: "已复制 ✓",
  binaries: "预编译包",
  chinaMirrors: "中国镜像",
  installGuideLink: "阅读安装指南",
  communityHeading: "为 Codewhale 做贡献",
  communityBody: "已有 {count} 人参与构建 Codewhale。在 GitHub 上报告问题、改进文档或贡献代码。",
  communityLinksAria: "社区链接",
  contribute: "在 GitHub 上参与贡献",
};
