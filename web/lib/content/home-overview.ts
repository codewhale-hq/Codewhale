import type { LocalizedText } from "./vocabulary";

export const HOME_OVERVIEW = {
  modelsCount: { en: "{version} has {routes} provider routes and {models} catalog models.", zh: "{version} 包含 {routes} 条提供商路由和 {models} 个目录模型。" },
  modelsAny: { en: "Any OpenAI-compatible endpoint", zh: "任何 OpenAI 兼容端点" },
  modelsNewest: { en: "Newest in the catalog", zh: "目录中最新的模型" },
  computerUseTitle: { en: "Computer Use", zh: "Computer Use" },
  computerUseBody: {
    en: "Let an agent work in Mac apps in the background while you keep using your computer.",
    zh: "让智能体在后台操作 Mac 应用，你可以继续使用电脑。",
  },
  computerUseTag: { en: "macOS beta", zh: "macOS 测试版" },
  pluginsTitle: { en: "Everything is a plugin", zh: "一切皆插件" },
  pluginsBody: {
    en: "Skills, commands, agent profiles, hooks and connectors to the apps you already use all arrive the same way: as a bundle you review, trust and enable.",
    zh: "技能、命令、智能体配置、钩子，以及连接你常用应用的连接器，都以同一种方式加入：作为你审查、信任并启用的插件包。",
  },
  pluginsLink: { en: "Browse plugins and connectors", zh: "浏览插件和连接器" },
  releaseLabel: { en: "Latest release {version}", zh: "最新版本 {version}" },
} satisfies Record<string, LocalizedText>;
