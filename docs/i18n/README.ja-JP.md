<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>モデルは誰のものでもかまいません。エージェントはあなたのものであるべきです。</h1>

Codewhale は、ターミナルで使うオープンソースのコーディングエージェントで、Rust で書かれています。どんなモデルでも動き、1 つのタスクを複数のエージェントに分担させられます。そのコードは 1 行残らず読めます。

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

これで macOS または Linux に最新リリースがインストールされ、アカウントは不要です。[Windows、npm、Cargo](#インストール) は下にあります。

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[ウェブサイト](https://codewhale.net) · [ドキュメント](../README.md) · [変更履歴](../../CHANGELOG.md) · [コントリビュート](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>この翻訳は機械翻訳による下書きで、ネイティブによるレビューはまだありません。</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="デモタスクを終えたターミナルの Codewhale。export.py に --json フラグを追加し、ローカルテスト 3 件がすべて通り、委任したレビューで問題が見つからなかったと報告しています。下のタスク一覧には完了した 3 つのステップが並び、ステータス行には Full Access とローカルの website-demo プロバイダーが表示されています。">

<sub>スクリプト化したテストモデルを使う 0.10.3（未リリース）の開発ビルドを、使い捨てのワークスペースで Full Access で動かしたものです。編集、3 件のテスト、サブエージェントによるレビューは実際に実行されました。</sub>

</div>

## インストール

上のコマンドは、チェックサムで検証した `codewhale` と短い別名 `codew` を `~/.local/bin` に置きます。その後シェルが `command not found` と表示したら、インストーラーが表示する PATH の案内に従ってください（[詳細](../INSTALL.md#put-it-on-your-path)）。`codewhale update` でアップグレードできます。

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

ビルド済みバイナリは Linux、macOS、Windows の x64 と arm64 に対応しています。Scoop、Nix、Linux の Homebrew、Android と Termux（プレビュー）、手動ダウンロードは[インストールガイド](../INSTALL.md)に、コンテナーは [Docker](../DOCKER.md) にあります。インストール方法は 1 台につき 1 つにしてください。そうしないと `PATH` に 2 つのコピーが入り、先にある方が使われます。

## クイックスタート

1. 作業をコミットまたはスタッシュしてから、git リポジトリのルートで `codewhale` を実行します。既定では、リポジトリ内のファイル編集は確認なしで適用し、読み取り専用でないシェルコマンドは実行前に毎回確認します。
2. `/provider` を実行（または `F3` を押す）し、プロバイダーを選んで API キーを貼り付け、モデルを選びます。既定のプロバイダーは DeepSeek です。キーがない状態で初めて起動すると、最初のメッセージを送ったときにプロバイダーを尋ね、ローカルの Ollama サーバーが応答すればローカルモデルを提案します。
3. タスクを入力します：

```text
Fix the failing tests and explain what changed.
```

読み取り専用のコマンドはすぐに実行されます。それ以外のシェルコマンドは先に確認されます。`y` で今回だけ実行、`a` でこの会話の間は同じようなコマンドを許可、`n` で拒否です。`/undo` は直前のリクエストとそのファイル変更を取り消し、`/help` はコマンドとキーを一覧表示します。

スクリプトや CI では、`codewhale exec` がインターフェースなしで 1 つのタスクを実行します。`--auto` でツールを渡す前に[ヘッドレス実行](../INSTALL.md#headless-scripts-ci)を読んでください。サンドボックスを指定しない限り、`--auto` は Full Access を使うからです。

## できること

Codewhale にタスクを渡すと、タスクが終わるか入力が必要になるまで、リポジトリを読み、ファイルを編集し、コマンドを実行します。[最新リリースのリリースノート](https://github.com/codewhale-hq/Codewhale/releases/latest)に最新の変更があり、[変更履歴](../../CHANGELOG.md)には全バージョンが載っています。

### どんなモデルでも

Anthropic、OpenAI、DeepSeek、Google、Mistral、Moonshot、OpenRouter、xAI など 40 以上のプロバイダーを内蔵しています。OpenAI 互換のエンドポイントならどれでも使え、Ollama、vLLM、SGLang を通したローカルモデルも使えます。既定のモデルは DeepSeek の `deepseek-flash` で、現在は DeepSeek V4.1 Flash が応答します（[詳細](../PROVIDERS.md#deepseek-v41-flash)）。

`/model` はターンの合間にモデルを切り替え、`/model auto` はターンごとにモデルを選びます（選び方は `/router` で設定）。`/turn inspect` は、あるターンがどのモデルを使い、いくらかかり、どれだけ時間がかかったかを表示します。[プロバイダー](../PROVIDERS.md)を参照してください。

### レシートと取り消し

`/receipts`（終了後は `codewhale receipts --last`）は、セッションが変更したファイル、実行したコマンド、ウェブと MCP の呼び出し、起動したエージェント、各承認とその承認者、失敗した操作を一覧にします。`/undo` は直前のリクエストと変更したファイルを 1 回で取り消します。`/restore` はワークスペース全体を以前のスナップショットに戻します。後の編集を上書きするため、`/trust on` か Full Access が必要です。[レシート](../RECEIPTS.md)を参照してください。

### 1 つのタスクに複数のエージェント

`/goal` はターンをまたいで続く目標を設定します。Codewhale は作業を[サブエージェント](../SUBAGENTS.md)に分け、それぞれの役割とモデルを持つエージェントの [Fleet](../FLEET.md) を動かし、リポジトリにコミットされた[ワークフロー](../WORKFLOW_AUTHORING.md)に従うこともできます。

### 拡張

[MCP サーバー](../MCP.md)を接続し、[スキル](../SKILLS.md)と[プラグイン](../PLUGINS.md)を追加し、セッションやツールのイベントで[フック](../HOOKS.md)を実行できます。コードモード（実験的、既定で有効）では、モデルが複数の MCP ツールやプラグインツールを呼び出す `execute_tools` プログラムを 1 つ書けます。各呼び出しはあなたの承認ルールに従い、レシートも残ります。v0.10.2 に同梱の Computer Use プラグイン（実験的、macOS のみ）は他のアプリを観察・操作でき、あなたがアクセス権を確認して有効にするまではオフのままです（[ガイド](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)）。0.10.3 で予定：このプラグインは[別途リリース](https://github.com/codewhale-hq/codewhale-cu-plugin)されます。

### 実行方法

| コマンド | 内容 |
| --- | --- |
| `codewhale` | 対話型のターミナルアプリ |
| `codewhale exec "…"` | インターフェースなしで 1 つのタスクを実行（スクリプトや CI 向け、[ヘッドレス実行](../INSTALL.md#headless-scripts-ci)） |
| `codewhale web` | `127.0.0.1` で動くローカルのウェブクライアント（[ガイド](../WEB.md)） |
| `codewhale review --pr N` | GitHub のプルリクエストへの助言的なレビュー。[GitHub ボットの設定](../GITHUB_ACTION.md#set-up-your-own-bot)を使うと、あなた自身のモデルキーで動かせます |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md)。スレッド、イベント、承認、各ターンのファイル変更を扱うローカル HTTP API |
| `codewhale serve --acp` | stdio 上の Agent Client Protocol。Zed などのエディター向け |

ターミナルアプリ、`exec`、`web`、Runtime API は 1 つのエンジンを共有しています（[アーキテクチャ](../ARCHITECTURE.md)）。コミュニティが保守する [VS Code 拡張機能](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)はサイドバーから接続します（[ソース](https://github.com/HengQuWorld/CodeWhale-VSCode)）。

## 安全を保つしくみ

2 つの切り替えで、Codewhale が確認なしにどこまで行うかが決まります。入力欄が空のとき `Tab` はモードを切り替えます。**Plan** は読み取りと検索だけで何も変更せず、**Work** はファイルを編集しコマンドを実行し、**Operate** は Work と同じ権限で、目標が検証されるかあなたの入力が必要になるまで作業を続けます。`Shift+Tab` は権限を切り替えます。既定の **Ask** はリポジトリ内のファイル編集を確認なしで適用し、読み取り専用でないシェルコマンドの前に確認します。**Auto-Review** はルールとレビュー用モデルに日常的な呼び出しを承認させ、高リスクなものは拒否します。それらを自分で承認するには Ask に切り替えます。**Full Access** は一切確認せず Codewhale のサンドボックスを無効にしますが、リポジトリのルールと管理ポリシーは引き続き適用されます。詳しくは [v0.10.2 のガイド](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md)をご覧ください。

0.10.3 で予定：`Shift+Tab` は Ask、Accept Edits、Plan、Auto-Review、Full Access を順に切り替える 1 つのダイヤルになり、`Tab` はモードを切り替えなくなり、Operate は廃止されます。Ask はファイル編集の前にも毎回確認するようになり、新しい既定の Accept Edits は git リポジトリ内の編集を適用し、コマンドの前には引き続き確認します。[現在のガイド](../MODES.md)で説明しています。

Ask と Auto-Review では、システムに OS のサンドボックスがあればその中でコマンドを実行します。macOS では Seatbelt、Linux では bubblewrap がインストールされていればそれを使います。Windows にはまだ OS のサンドボックスがなく、Full Access ではサンドボックスがオフになります。サンドボックス内では、シェルコマンドとターミナルセッションは `~/.ssh`、`~/.aws`、`.env` ファイルなどの資格情報ストアを読めません。`/preview-request` は次のリクエストを送らずに、伏せ字にしたプレビューを表示します。

アカウントは不要で、`/provider` で追加したキーはあなたのマシンに残ります。自分の API キーを使うと料金はプロバイダーに直接支払い、ローカルモデルにはキーが要りません。料金がわかる場合はフッターと `/cost` にセッションの費用が表示され、不明な料金は不明と表示され、無料とは表示されません。

テレメトリーは既定でオンです。集計した数値（バージョンとプラットフォーム、セッションの長さと結果、使った内蔵プロバイダー、機能とエラーの回数）を、90 日ごとに変わるランダムなインストール ID とともに送信し、会話、コード、プロンプト、モデル名、ファイル名やリポジトリ名、資格情報は決して送りません。次のコマンドでオフにできます：

```bash
codewhale config set telemetry false
```

Codewhale は起動時に GitHub で新しいリリースも確認します。`CODEWHALE_NO_UPDATE_CHECK=1` を設定すると確認しません。詳しくは[認可の順序](../AUTHORIZATION_ORDER.md)、[サンドボックス](../SANDBOX.md)、[テレメトリー](../TELEMETRY.md)をご覧ください。

## これからの方向

Codewhale は、ターミナル、デスクトップアプリ、ブラウザー、クラウドコンピューターにまたがる一つの環境になりつつあり、すべてが一つのエンジンで動きます。以下の各部分に、今日の状況を示しています。

| 部分 | 状況 | 現在 |
| --- | --- | --- |
| ターミナルアプリ | リリース済み | macOS、Linux、Windows にインストールし、任意のリポジトリで `codewhale` を実行します。 |
| ローカルブラウザークライアント | リリース済み | `codewhale web` を実行すると、ブラウザーで会話を読み、タスクを送り、承認に応答できます。ターミナルに付属し、アカウントは不要です。 |
| 1 つのタスクに多数のエージェント | リリース済み | サブエージェント、Fleet、ワークフローにより、複数のエージェントがそれぞれの役割で 1 つのタスクを分担できます。 |
| デスクトップアプリ | 開発中、ダウンロードなし | 同じエンジンの上でデスクトップアプリを構築中で、公開ダウンロードはまだありません。 |
| クラウドコンピューター | 招待制 | クラウドコンピューターは招待制で、セッションがクラウド上のマシンで作業を続けられるよう構築中です。 |
| それらをつなぐ 1 つのセッション | 構築中 | ターミナルからデスクトップ、ブラウザー、クラウドまで、あなたについていき、最後まであなたのものである 1 つのセッションを構築中です。 |

[ロードマップ](../ROADMAP.md)には、各リリースの範囲と手伝える場所が書かれています。

## ほかのエージェントから移ってくる方へ

Codewhale は `AGENTS.md` を自動で読みます。`CLAUDE.md`、`.cursorrules`、`.github/copilot-instructions.md` など、ほかのツール向けに書かれた指示ファイルは、設定で `project_instruction_imports = ["claude"]`（または `"cursor"`、`"cline"`、`"windsurf"`、`"copilot"`、`"all"`）を指定したときだけ読み込まれます。[config.example.toml](../../config.example.toml) を参照してください。読み込まなかったファイルを見つけると Codewhale は警告します。

`/import-claude` は Claude Code の設定と MCP サーバーの取り込みを準備し、あなたが確認できるようにします。`/plugin install` は、スキル、コマンド、エージェントプロファイル、MCP サーバーを含む Claude Code プラグインを受け入れ、フックや LSP サーバーが必要なプラグインは拒否します（[詳細](../CLAUDE_PLUGIN_COMPAT.md)）。

## ドキュメント

これらのガイドはメインブランチに沿っており、最新リリースにまだ含まれていない変更を説明している場合があります。

| はじめに | リファレンス |
| --- | --- |
| [ユーザーガイド](../GUIDE.md) | [設定](../CONFIGURATION.md) |
| [インストール](../INSTALL.md) | [アーキテクチャ](../ARCHITECTURE.md) |
| [プロバイダーとローカルモデル](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [モードと権限](../MODES.md) | [プラグインの作成](../PLUGIN_AUTHORING.md) |
| [キー割り当て](../KEYBINDINGS.md) | [レシート](../RECEIPTS.md) |
| [GitHub のプルリクエストレビュー](../GITHUB_ACTION.md) | [すべてのドキュメント](../README.md) |

## コントリビュート

Codewhale は、使う人たちによって、たくさんの言語でオープンに作られています。

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale のコントリビューターウォール。Codewhale にコード、共同作者のコミット、引き継がれたプルリクエスト、クレジットされた報告がある人それぞれのアバターが並び、上部に合計人数があります。"></a>

[コントリビューターウォール](../CONTRIBUTORS.md)は、Codewhale にコード、共同作者のコミット、引き継がれたプルリクエスト、クレジットされた報告があるすべての人を数えており、GitHub 自身のコントリビューター一覧が見落とす人も含みます。`scripts/contributor-wall.py` が git の履歴と GitHub から画像と一覧を作り直します。

バグ、アイデア、見た目や使い心地の違和感など、どんなフィードバックも歓迎です。どれも [issue を作成](https://github.com/codewhale-hq/Codewhale/issues/new/choose)してください。新機能を始めるのにも issue がいちばんです。修正、翻訳、プロバイダー、計画済みの作業のプルリクエストも歓迎です。質問は [Discussions](https://github.com/codewhale-hq/Codewhale/discussions) へどうぞ。[コントリビューションガイド](../../CONTRIBUTING.md)には、手伝い方ごとの行き先、あなたの名前で作業が取り込まれる流れ、[どこに何があるか](../../CONTRIBUTING.md#project-structure)が書かれています。

[Discord](https://discord.gg/37gfS3ksug) で話すか、メンテナーの Hunter を WeChat（`hunterbown`）で追加して Whale Brothers グループへの参加を頼んでください。どなたでも歓迎です。

## 経緯とライセンス

Codewhale は `deepseek-tui` として始まり、今もそのプロジェクトの設定とセッションを読み込めます。独立して保守されており、DeepSeek やほかのモデルプロバイダーとは提携していません。

[MIT](../../LICENSE)。ほかのオープンソースプロジェクトから取り入れたコードは、第三者通知の[同梱したソース](../../THIRD_PARTY_NOTICES.md)と[取り入れた設計](../THIRD_PARTY_NOTICES.md)に記載しています。
