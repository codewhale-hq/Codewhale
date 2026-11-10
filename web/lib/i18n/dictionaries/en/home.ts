import type { HomeDict } from "../types";

/** English reference home copy: useful work with chosen models and connected tools. */
export const home: HomeDict = {
  metaTitle: "Codewhale · Use any model through an agent you can read",
  metaDescription: "Codewhale is the open-source agent between you and any model. Read its code, or ask it to explain itself. An international community improves it with every release.",

  heroTitle: "Use any model through an agent you can read",
  heroIntro: "{brand} is the open-source agent between you and the model. Read its code, or ask it to read its own code and explain what it does. An international community improves it with every release.",
  getCodewhale: "Install Codewhale",
  heroInstallAria: "Install command",
  exploreProduct: "Explore Codewhale",

  shotPreview: "Local terminal demo",
  screenshotAlt:
    "Codewhale v{version}: a captured terminal view with the conversation, message composer and session controls.",

  latestRelease: "Latest release {tag}",
  releaseUnavailable: "Release status unavailable",
  proofContributors: "{count} contributors",
  currentSource: "Source",
  sourceCandidate: "Unreleased",
  publishedRelease: "released",

  gainHeading: "Put your ideas to work",
  gainLede:
    "Start with a project and the result you need. Codewhale can read the files, make changes and run commands to check its work.",
  gain: [
    [
      "Build and repair software",
      "Add a feature, trace a bug or write a useful tool. Work through the changes with an agent that can edit the project and run its checks."
    ],
    [
      "Automate work you repeat",
      "Turn a repeated task into a script or run it with codewhale exec. For larger jobs, give parts of the work to a Fleet of agents with different models."
    ],
    [
      "Connect the tools you use",
      "Bring in tools through plugins and MCP servers. Connect the services your work needs, with access and authentication you configure."
    ]
  ],

  chapterModels: "Your models",
  modelsHeading: "Use the models you choose",
  modelsBody:
    "Use provider API keys, an OpenAI-compatible endpoint or local models.",
  modelsFacts: [
    [
      "Your API accounts",
      "Use your own API keys with providers such as DeepSeek, OpenAI, Anthropic and Google. Your provider bills the model usage."
    ],
    [
      "Your gateway",
      "Use an OpenAI-compatible endpoint and choose the models it serves."
    ],
    [
      "Your inference",
      "Connect Ollama on your machine, or a self-hosted endpoint such as vLLM or SGLang."
    ],
    [
      "Your ChatGPT plan",
      "Experimental: connect an eligible ChatGPT plan with codewhale auth chatgpt."
    ]
  ],
  modelsLink: "Browse models and providers",

  startHeading: "Start with one useful task",
  startLede:
    "Install Codewhale, connect a model and open a project folder. You can add plugins and more agents as you need them.",
  startGuideLink: "Follow the getting-started guide",
  startVocabularyLink: "Look up a term",

  chapterAvailability: "Where it runs",
  availabilityHeading: "Available now and in development",
  availabilityLede:
    "Install on macOS, Linux or Windows. Work in your terminal, or run codewhale web for a local browser view. Desktop and hosted web apps are in development.",
  availability: [
    [
      "Terminal and local browser",
      "Released",
      "Install on Linux, macOS, or Windows, then run codewhale, or codewhale web for the local browser client. npm and Cargo also work; Android on Termux is a preview.",
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Available",
      "A separate, community-maintained project: chat, threads, and file changes in a VS Code sidebar over the same Codewhale Runtime. Install it from the VS Code Marketplace.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode",
    ],
    [
      "Hosted web app",
      "In development",
      "Being rebuilt to match the desktop app, and not open yet. For a browser view today, run codewhale web on your machine.",
    ],
    [
      "Desktop",
      "Development build",
      "A native app on the same engine. Review and commit changes, follow work across projects, and edit a message to resend it as a new branch. No public download yet.",
    ],
  ],
  availabilityNote:
    "The terminal, local browser, and GUI need no Codewhale account. When you use your own provider key, your provider bills that usage.",

  surfacesHeading: "Work across files and tools",
  surfaces: [
    [
      "Files and terminal",
      "Create files, run commands, inspect data and test what you build. You set the working folder and permissions."
    ],
    [
      "Plugins and connected apps",
      "Add skills and tools through plugins and MCP. Review and enable the connections you want the agent to use."
    ],
    [
      "Browser and computer · preview",
      "Add browser tools or the Computer Use plugin to work in apps and websites. Computer Use on macOS needs system permissions; other platforms remain experimental."
    ],
    [
      "Sessions you can return to",
      "Keep the conversation, tool results and work history together. Resume the task in the terminal or its local browser client."
    ],
    [
      "Teams of agents",
      "Use Fleet to split a larger job between agents with different roles and models, and follow their progress in one place."
    ]
  ],
  runtimeLink: "Explore tools and integrations",

  installBandHeading: "Install on macOS or Linux",
  copy: "Copy",
  copied: "Copied ✓",
  binaries: "Binaries",
  chinaMirrors: "China mirrors",
  installGuideLink: "Read the install guide",

  communityHeading: "Contribute to Codewhale",
  communityBody:
    "{count} contributors. Code, docs, translations and skills all help.",
  communityLinksAria: "Community links",
  contribute: "Contribute",
};
