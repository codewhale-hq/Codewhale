import type { HomeDict } from "../types";

/** English reference home copy: useful work with chosen models and connected tools. */
export const home: HomeDict = {
  metaTitle: "Codewhale: an open-source coding agent",
  metaDescription: "Codewhale is an open-source coding agent for your terminal. Read, edit and test projects with your own hosted or local models.",

  heroTitle: "The open-source coding agent for any model",
  heroIntro: "{brand} reads your project, edits files and runs your tests from your terminal. Connect a hosted or local model, and choose which actions need your approval.",
  getCodewhale: "Install Codewhale",
  heroInstallAria: "Install command",
  exploreProduct: "Explore Codewhale",

  shotPreview: "Terminal preview",
  screenshotAlt:
    "Codewhale v{version}: a captured terminal view with the conversation, message composer and session controls.",

  latestRelease: "Latest release {tag}",
  releaseUnavailable: "Release status unavailable",
  proofContributors: "{count} contributors",
  currentSource: "Source",
  sourceCandidate: "Unreleased",
  publishedRelease: "released",

  gainHeading: "Build and automate from your terminal",
  gainLede:
    "Describe what you want to build or automate. Codewhale can edit files, run commands and check the result, with access you control.",
  gain: [
    [
      "Build apps and tools",
      "Create an app, add a feature or write a script. Codewhale can work through the project files, run the code and test what it builds."
    ],
    [
      "Automate the work you repeat",
      "Run workflows from your terminal, scripts or CI. For larger tasks, delegate parts of the work to a Fleet of agents with different models."
    ],
    [
      "Connect the tools you use",
      "Add tools through plugins and MCP servers, or use APIs from your own scripts. Each service needs its own setup and authentication."
    ]
  ],

  chapterModels: "Your models",
  modelsHeading: "Use the models you choose",
  modelsBody:
    "Connect your provider accounts, an OpenAI-compatible endpoint, or local and self-hosted models. Pick a model for the session and for each agent in a Fleet.",
  modelsFacts: [
    [
      "Your API accounts",
      "Connect providers such as DeepSeek, OpenAI, Anthropic or Google with your own keys. The default, deepseek-flash, now runs DeepSeek-V4.1-Flash."
    ],
    [
      "Your gateway",
      "Use an OpenAI-compatible endpoint and choose the models it serves."
    ],
    [
      "Your inference",
      "Run local or self-hosted models with Ollama, vLLM or SGLang."
    ],
    [
      "Your ChatGPT plan",
      "Sign in with codewhale auth chatgpt to use a ChatGPT plan instead of an API key. Experimental."
    ]
  ],
  modelsLink: "Browse models and providers",

  startHeading: "Get started",
  startLede:
    "Install Codewhale, connect a model and open a project folder. You can add plugins and more agents as you need them.",
  startGuideLink: "Follow the getting-started guide",
  startVocabularyLink: "Look up a term",

  chapterAvailability: "Where it runs",
  availabilityHeading: "Available now and in development",
  availabilityLede:
    "The terminal and local browser client are available now. A native desktop app and a rebuilt hosted web app are in development.",
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
      "Use browser tools and the Computer Use plugin for work in apps and websites, with the access you grant."
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
    "{count} people have built Codewhale. Report a bug, improve the docs or contribute code on GitHub.",
  communityLinksAria: "Community links",
  contribute: "Contribute on GitHub",
};
