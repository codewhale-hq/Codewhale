import type { ChromeDict } from "../types";

/**
 * Simplified Chinese chrome — a native rewrite mirroring the current
 * English direction (the wordmark tag is "any model, on your machine";
 * the retired "local-first" framing is gone).
 */
export const chrome: ChromeDict = {
  navDocs: "文档",
  navContribute: "贡献",
  navSignIn: "登录",
  navDesktop: "桌面版",

  navFeatures: "功能",
  navModels: "模型",
  navPlugins: "插件",

  skipToContent: "跳转到主要内容",

  navPrimaryAria: "主导航",
  navHomeAria: "Codewhale 首页",

  installCta: "安装 →",

  dateLocale: "zh-CN",

  menuOpen: "打开菜单",
  menuClose: "关闭菜单",
  translationMachineDraft: "本翻译为机器草稿，尚未经母语者审校。",

  themeAuto: "自动",
  themeLight: "浅色",
  themeDark: "深色",
  themeAria: "主题：{mode}（点击切换）",
  themeTitle: "主题 · 自动 / 浅色 / 深色",

  footerProduct: "产品",
  footerProject: "项目",
  footerDocs: "文档",
  footerGuide: "新手指引",
  footerInstall: "安装",
  footerModels: "模型",
  footerFaq: "常见问题",
  footerContribute: "参与贡献",
  footerRoadmap: "路线图",
  footerLicense: "MIT 许可证",
  footerMerch: "周边商品",
  footerTerms: "服务条款",
  footerPrivacy: "隐私政策",
  footerChangelog: "更新日志",
  footerCanonicalSource: "官方源码：",
  footerReleases: " · 发布：",
  footerReleasesLink: "GitHub 发布页",
  footerSecurity: "安全",
  footerLegal: "法律",

  switcherLabel: "语言",
  switcherSwitchTo: "切换到 {label}",
  partialBadge: "(部分)",

  stateReleased: "已发布",
  stateAvailable: "可用",
  stateBeta: "测试版",
  stateDev: "开发版本",
  stateOpen: "已开放",
  stateInviteOnly: "仅限受邀",
  stateNotOpen: "暂未开放",
  stateNotPublic: "未公开",
  stateInSource: "已在源码中，尚未发布",
  numbersAsOf: "截至 {date}",
  downloadMacos: "下载 macOS 版",
  downloadVersion: "版本 {version}",
  downloadSize: "大小 {size}",
  downloadChecksum: "SHA-256 校验和",
  downloadSigned: "已签名并经过公证",
};
