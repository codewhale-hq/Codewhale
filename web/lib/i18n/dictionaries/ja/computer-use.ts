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
  lead: "あなたが作業を続けている間に、Codewhale がアプリを操作します。この Mac で一度許可すれば、コンピュータ操作はいつでも停止できます。",
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
    { title: "Computer Use を有効にする", body: "Codewhale の Connectors 設定で Computer Use をインストールまたは更新し、内容を確認して有効にします。アプリでの作業を依頼すると、初回にネイティブヘルパーが表示されます。Trust until revoked を選ぶと、アプリ、モデル、タスクが変わっても信頼が維持されます。" },
  ],
  controlsTitle: "いつでも操作を管理",
  controlsBody: "操作対象のアプリのそばに、Codewhale の小さな操作パネルが表示されます。Pause はコンピュータ操作を一時停止し、Stop computer use は現在のすべてのコンピュータ操作セッションを終了します。アクセスを取り消すには、ヘルパーで Turn off を選びます。作業の様子を見たいときは、ライブプレビューを依頼してください。",
  updateTitle: "アップデートはあなたのタイミングで",
  updateBody: "アプリから Check for updates（アップデートを確認）を選択します。インストール前にダウンロードしたファイル、Codewhale の署名、Apple の公証を検証し、復旧用に以前のアプリを保持します。",
  help: "セットアップとトラブルシューティング",
  notes: "リリースノート",
  demo: "バックグラウンドチェックを見る",
  source: "ソースコードとその他のプラットフォーム",
  platforms: "このダウンロードは Mac 用です。Windows と Linux では現在、ソースプラグインとホスト側のセットアップを使用します。",
};
