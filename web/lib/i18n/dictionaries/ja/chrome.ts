import type { ChromeDict } from "../types";

/**
 * Japanese chrome dictionary.
 *
 * Native rewrite mirroring the current English direction — "any model, on
 * your machine", not the retired "local-first" positioning.
 *
 * Terminology follows the TUI locale pack (`crates/tui/locales/ja.json`):
 * modes and permission names stay literal (Plan / Work / Operate, Ask /
 * Auto-Review / Full Access), 権限 is "permissions", 推論 is "reasoning",
 * レシート is "receipt". The pack renders "posture" as 姿勢/権限 rather than
 * the katakana calque ポスチャ, so the website matches it.
 */
export const chrome: ChromeDict = {
  navDocs: "ドキュメント",
  navStart: "はじめに",
  navInstall: "インストール",
  navFaq: "よくある質問",
  navCommunity: "コミュニティ",
  navContribute: "貢献",

  navFeatures: "機能",
  navProduct: "製品",
  navModels: "モデル",
  navPlugins: "プラグイン",

  skipToContent: "メインコンテンツへスキップ",

  navPrimaryAria: "メインナビゲーション",
  navHomeAria: "Codewhale ホーム",

  installCta: "インストール →",

  dateLocale: "ja-JP",

  menuOpen: "メニューを開く",
  menuClose: "メニューを閉じる",

  themeAuto: "自動",
  themeLight: "ライト",
  themeDark: "ダーク",
  themeAria: "テーマ：{mode}（クリックで切り替え）",
  themeTitle: "テーマ · 自動 / ライト / ダーク",

  footerTagline:
    "ターミナルと好みのモデルで使えるオープンソースのコーディングエージェント。",
  footerProduct: "製品",
  footerProject: "プロジェクト",
  footerDocs: "ドキュメント",
  footerGuide: "はじめかた",
  footerInstall: "インストール",
  footerModels: "モデル",
  footerRuntime: "ランタイム",
  footerFaq: "よくある質問",
  footerIssues: "Issues",
  footerContribute: "貢献",
  footerRoadmap: "ロードマップ",
  footerLicense: "MIT ライセンス",
  footerTerms: "利用規約",
  footerPrivacy: "プライバシーポリシー",
  footerChangelog: "変更履歴",
  footerCanonicalSource: "正規ソース：",
  footerReleases: " · リリース：",
  footerReleasesLink: "GitHub リリース",
  footerSecurity: "セキュリティ連絡先",

  switcherLabel: "言語",
  switcherSwitchTo: "{label} に切り替え",
  partialBadge: "（一部翻訳）",
};
