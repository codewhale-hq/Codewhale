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
  navContribute: "貢献",
  navSignIn: "ログイン",
  navDesktop: "デスクトップ",

  navFeatures: "機能",
  navModels: "モデル",
  navPlugins: "プラグイン",

  skipToContent: "メインコンテンツへスキップ",

  navPrimaryAria: "メインナビゲーション",
  navHomeAria: "Codewhale ホーム",

  installCta: "インストール →",

  dateLocale: "ja-JP",

  menuOpen: "メニューを開く",
  menuClose: "メニューを閉じる",
  translationMachineDraft: "この翻訳は機械翻訳による下書きで、ネイティブによるレビューはまだありません。",

  themeAuto: "自動",
  themeLight: "ライト",
  themeDark: "ダーク",
  themeAria: "テーマ：{mode}（クリックで切り替え）",
  themeTitle: "テーマ · 自動 / ライト / ダーク",

  footerProduct: "製品",
  footerProject: "プロジェクト",
  footerDocs: "ドキュメント",
  footerGuide: "はじめかた",
  footerInstall: "インストール",
  footerModels: "モデル",
  footerFaq: "よくある質問",
  footerContribute: "貢献",
  footerRoadmap: "ロードマップ",
  footerLicense: "MIT ライセンス",
  footerMerch: "グッズ",
  footerTerms: "利用規約",
  footerPrivacy: "プライバシーポリシー",
  footerChangelog: "変更履歴",
  footerCanonicalSource: "正規ソース：",
  footerReleases: " · リリース：",
  footerReleasesLink: "GitHub リリース",
  footerSecurity: "セキュリティ連絡先",
  footerLegal: "法務",

  switcherLabel: "言語",
  switcherSwitchTo: "{label} に切り替え",
  partialBadge: "（一部翻訳）",

  stateReleased: "リリース済み",
  stateAvailable: "利用可能",
  stateBeta: "ベータ",
  stateDev: "開発ビルド",
  stateOpen: "公開中",
  stateInviteOnly: "招待制",
  stateNotOpen: "まだ公開していません",
  stateNotPublic: "非公開",
  stateInSource: "ソースにあり、未リリース",
  numbersAsOf: "{date} 時点",
  downloadMacos: "macOS 版をダウンロード",
  downloadVersion: "バージョン {version}",
  downloadSize: "サイズ {size}",
  downloadChecksum: "SHA-256 チェックサム",
  downloadSigned: "署名・公証済み",
};
