import type { HomeDict } from "../types";

/** Simplified Chinese home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale · 你电脑上的开源智能体",
  metaDescription: "在终端里用开源智能体构建应用、修复代码并自动完成工作。选择托管或本地模型，通过代码、文档和反馈一起改进 Codewhale。",
  heroTitle: "你电脑上的开源智能体",
  heroIntro: "在终端中用 {brand} 开发软件、运行可复用的工作流。添加 Computer Use，即可在 Mac 应用中执行任务。",
  getCodewhale: "安装 Codewhale",
  heroInstallAria: "安装命令",
  exploreProduct: "探索 Codewhale",
  shotPreview: "本地终端演示",
  screenshotAlt:
    "Codewhale v{version} 的终端实录画面，包含对话、消息输入框与会话控制。",
  latestRelease: "最新发布 {tag}",
  releaseUnavailable: "发布状态暂不可用",
  proofContributors: "{count} 位贡献者",
  currentSource: "源码",
  sourceCandidate: "未发布",
  publishedRelease: "已发布",
  gainHeading: "把想法变成成果",
  gainLede: "打开项目，说出你需要的结果。Codewhale 可以读取文件、修改内容，并运行命令检查自己的工作。",
  gain: [
    [
      "构建与修复软件",
      "添加功能、排查问题，或编写实用工具。与能修改项目并运行检查的智能体一起完成工作。"
    ],
    [
      "让重复工作自动完成",
      "从终端、脚本或 CI 中运行工作流。任务较大时，可以把部分工作交给使用不同模型的 Fleet 智能体团队。"
    ],
    [
      "连接你常用的工具",
      "通过插件和 MCP 服务器接入工作所需的工具与服务。访问范围和身份验证由你配置。"
    ]
  ],
  chapterModels: "你的模型",
  modelsHeading: "使用你选择的模型",
  modelsBody:
    "使用提供商 API 密钥、OpenAI 兼容端点或本地模型。",
  modelsFacts: [
    [
      "你的 API 账户",
      "使用自己的 API 密钥连接 DeepSeek、OpenAI、Anthropic、Google 等提供商。模型用量由提供商计费。"
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
      "实验功能：运行 codewhale auth chatgpt，连接符合条件的 ChatGPT 套餐。"
    ]
  ],
  modelsLink: "浏览模型与提供商",
  startHeading: "从一件有用的任务开始",
  startLede: "安装 Codewhale，连接模型，然后打开项目文件夹。需要时，再添加插件或更多智能体。",
  startGuideLink: "按照新手指引操作",
  startVocabularyLink: "查名词",
  chapterAvailability: "在哪里运行",
  availabilityHeading: "当前可用与开发中的功能",
  availabilityLede: "在 macOS、Linux 或 Windows 上安装。在终端里工作，或运行 codewhale web 打开本地浏览器界面。桌面应用和托管网页应用仍在开发。",
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
      "添加浏览器工具或 Computer Use 插件，操作应用与网站。macOS 上的 Computer Use 需要系统权限；其他平台仍为实验支持。"
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
  communityHeading: "参与 Codewhale",
  communityBody: "已有 {count} 位贡献者。代码、文档、翻译和技能都欢迎。",
  communityLinksAria: "社区链接",
  contribute: "参与贡献",
};
