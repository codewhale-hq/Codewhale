import type { ComputerUseDict } from "../types";

/**
 * Japanese dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as the
 * app shows them; the macOS setting and folder names use the labels of the
 * Japanese system UI (アクセシビリティ / 画面収録 / アプリケーション).
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · プレビュー · Codewhale",
  metaDescription: "Mac 版 Codewhale Computer Use をダウンロードして設定します。アプリのバックグラウンド操作、権限の設定、そして人が操作する Pause と Stop のコントロールを備えています。",
  title: "Computer Use",
  lead: "Mac を使い続けながら、エージェントにアプリでの作業を任せられます。",
  publisher: "Codewhale 提供",
  download: "Mac 版をダウンロード",
  downloadZip: "ZIP アーカイブ（アプリ内アップデーターが使用）",
  requirements: "macOS 13.5 以降 · Apple silicon と Intel に対応",
  included: "ダウンロードするのはアプリひとつだけ。Node やコンパイラーを別途インストールする必要はありません。",
  pendingTitle: "Mac 版ダウンロードを準備中",
  pendingBody: "Apple の公証とリリース前チェックが完了し次第、公開インストーラーをここで提供します。",
  unavailableTitle: "ダウンロードの提供状況を確認できませんでした",
  unavailableBody: "ページを再読み込みして再試行するか、下の公開済みリリースをご確認ください。",
  releases: "公開済みリリース",
  receipt: "ダウンロードの検証情報",
  setup: "Mac をセットアップする",
  steps: [
    { title: "アプリをインストールする", body: "ディスクイメージを開き、Codewhale Computer Use を「アプリケーション」フォルダにドラッグします。「アプリケーション」から起動し、メニューバーのクジラのアイコンから Computer Use を選択してください。" },
    { title: "権限を確認する", body: "セットアップのボタンから、システム設定の「アクセシビリティ」と「画面収録」を開きます。どの権限を許可するかはあなたが決めます。" },
    { title: "エージェントを接続する", body: "CLI のバージョンに合う Codewhale の設定を下から選んでください。ほかの対応 MCP ホストでは、リポジトリの設定ガイドを参照してください。" },
  ],
  controlsTitle: "いつでも操作を管理",
  controlsBody: "macOS の権限を許可すると、標準モードではアプリごとの確認なしで動作します。拒否したアプリにはアクセスできません。Pause は操作を一時停止し、Stop computer use は現在のセッションを終了します。厳格な承認モードも選べます。作業を見たいときはライブプレビューを依頼してください。",
  updateTitle: "アップデートはあなたのタイミングで",
  updateBody: "アプリから Check for updates（アップデートを確認）を選択します。インストール前にダウンロードしたファイル、Codewhale の署名、Apple の公証を検証し、復旧用に以前のアプリを保持します。",
  help: "セットアップとトラブルシューティング",
  notes: "リリースノート",
  demo: "バックグラウンドチェックを見る",
  source: "GitHub で見る",
  platforms: "Mac ヘルパーはベータ版で、マーケットプレイスのプラグインは Mac のみに対応しています。公開リリースには署名のない実験的な Windows x64 プレビューもあります。Linux はソースまたは Docker による実験的なセットアップが必要です。Windows と Linux の入力は共有デスクトップを使用します。",
  connectTitle: "Codewhale に接続",
  connectVersion: "Codewhale 0.10.2 には旧版の Computer Use プラグインが内蔵されています。カタログからインストールしても更新されません。インストール済みの Mac ヘルパーに MCP で直接接続してください。",
  connectLimit: "MCP での直接接続はツールを追加しますが、プラグインのスキルは含みません。旧版の内蔵プラグインと併用しないでください。有効なら、先に /plugin disable computer-use を実行してください。",
  connectRuntime: "アプリには Node が含まれるため、別途インストールする必要はありません。別の場所にインストールした場合は、APP を実際のアプリバンドルのパスに設定してください。",
  connectReload: "実行中のターミナルセッションで /mcp reload を実行し、次のように依頼します：",
  connectPrompt: "request_access で Computer Use の権限を確認してください。入力操作は行わないでください。",
  connectExpected: "応答の via: app、ヘルパーのバージョン、不足している権限を確認してください。ヘルパーが見つからない場合は停止し、設定の案内に従ってください。これは接続と権限のみの確認です。",
  connectGuide: "MCP 設定ガイド",
  contributeTitle: "Computer Use の改善に参加",
  contributeBody: "ドキュメントの改善や、アプリの操作手順のテストと再現可能な結果の共有で協力できます。",
  contributeGuide: "貢献ガイド",
  contributeIssue: "問題を報告",
};
