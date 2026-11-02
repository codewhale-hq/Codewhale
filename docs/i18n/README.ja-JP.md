<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**どのモデルでも使える、ターミナル向けのオープンソースのコーディングエージェント。**

Codewhale にタスクを渡すと、タスクが完了するか、あなたの入力が必要になるまで、リポジトリを読み、ファイルを編集し、コマンドを実行します。
組み込みの 40 を超えるプロバイダー（既定は DeepSeek V4.1 Flash）に加え、自分のマシンで動かすモデルでも使えます。
`/receipts` は、変更したファイル、実行したコマンド、行った Web と MCP の呼び出しを一覧表示します。

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[ウェブサイト](https://codewhale.net) · [ドキュメント](../README.md) · [変更履歴](../../CHANGELOG.md) · [コントリビュート](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Codewhale のターミナルセッション" width="760">

<sub>スクリプト化したテストモデルを使う 0.10.2 の開発ビルドを、使い捨てのワークスペース内でフルアクセスにして実行した画面です。編集、3 つのテスト、サブエージェントによるレビューは実際に実行されています。</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — DeepSeek v4.1 Flash の利用量が 30% 増加" width="760">
  </a>
</p>

[Codewhale × OrcaRouter の特典を見る](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale は OrcaRouter から紹介料を受け取ります。

## インストール

macOS と Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

インストーラーは、チェックサム検証済みの `codewhale` と短縮エイリアス `codew` を `~/.local/bin` に配置します。
その後シェルが `command not found` と表示する場合は、インストーラーが出力する PATH 用の行を実行してください（[詳細](../INSTALL.md#put-it-on-your-path)）。
アップグレードは `codewhale update` で行えます。
これらのコマンドは最新のリリースをインストールします。
このページは `main` に合わせて更新されるため、バージョンが記された機能はそのリリースで提供されます。各リリースで追加される内容は[変更履歴](../../CHANGELOG.md)に記載しています。

<details>
<summary><b>Windows、npm、Cargo、その他のインストール方法</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

ビルド済みバイナリは、x64 と arm64 の Linux、macOS、Windows に対応しています。
Nix、Linux 上の Homebrew、Android/Termux（プレビュー）、手動ダウンロードについては[インストールガイド](../INSTALL.md)を、コンテナについては [Docker](../DOCKER.md) を参照してください。
インストール方法は 1 台のマシンにつき 1 つにしてください。
そうしないと `PATH` 上に 2 つのコピーができ、先に見つかった方が使われます。

</details>

## クイックスタート

1. 作業中の変更をコミットまたは stash してから、git リポジトリのルートで `codewhale` を実行します。
   既定では、そこでのファイル編集を確認なしで適用し、差分を表示します。
2. `/provider` を実行（または `F3` を押す）し、プロバイダーを選んで API キーを貼り付け、モデルを選びます。
   既定のプロバイダーは DeepSeek です。
   キーがない場合は、代わりにローカルランタイムを選びます。
   プロバイダーが未設定で、Ollama がすでにチャットモデルを提供している場合、Codewhale は自動的にそれを使います。
   0.10.2 からは、最初のメッセージを送信したときにそれを使うよう提案します。
3. タスクを入力します。

```text
Fix the failing tests and explain what changed.
```

読み取り専用のコマンドはすぐに実行されます。
それ以外のシェルコマンドは、Codewhale が先に確認します。
`y` で 1 回だけ実行、`a` でこの会話の残りの間は同様のコマンドを許可、`n` で拒否します。
`/undo` は直前のリクエストとそのファイル変更を元に戻します。
`/help` でコマンドとキーの一覧を表示します。

スクリプトや CI では、`codewhale exec` がインターフェースなしで 1 つのタスクを実行します。
既定では、`--auto` はフルアクセスを使います。ツールは確認なしで実行され、シェルコマンドには Codewhale のサンドボックスが適用されず、信頼モードによりファイルツールはワークスペースの外にも書き込めます。
`--sandbox workspace-write` を追加すると、信頼モードをオフにし、シェルのサンドボックスとファイルツールのワークスペース制限を維持できます。

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## モードと権限

新規インストール直後は、Work モード、権限は「確認」の状態で始まります。

| | 切り替え方法 | 選択肢 |
| --- | --- | --- |
| モード | `Tab`（入力欄が空のとき）または `/mode` | **Plan** は読み取りと検索を行いますが、何も変更しません。**Work** はファイルを編集し、コマンドを実行します。**Operate** は Work と同じアクセス権を持ち、リクエストを目標として扱い、目標が検証されるか、あなたの入力が必要になるまで作業を続けます。 |
| 権限 | `Shift+Tab` | **確認**（Ask）では、リポジトリ内のファイル編集は確認なしで適用され、読み取り専用ではないシェルコマンドの前に確認が入ります。**自動レビュー**（Auto-Review）では、ルールとレビュー担当のモデルが定型的な呼び出しを承認し、高リスクの呼び出しは拒否します。それらを自分で承認するには「確認」に切り替えてください。**フルアクセス**（Full Access）は一切確認せず、Codewhale のサンドボックスを無効にします。リポジトリのルールと管理ポリシーは引き続き適用されます。 |

詳しくは[モードと権限のガイド](../MODES.md)を参照してください。

## 主な機能

### 0.10.2 の新機能

- `/pet on` は、ターミナルをアニメーションのクジラに変え、エージェントが何をしているかを見せます。その間もメッセージ入力欄はそのまま使えます。
  980 個の粒子が、現在のステップを英語、中国語、日本語のいずれかで描き出します。
- `/workbar terminal` は、エージェントが実行中のシェルをリアルタイムで表示します。
  そこに入力することも、`Ctrl+N` で自分用のシェルを開くこともできます（macOS と Linux）。
- `/undo` は、直前のリクエスト全体を、変更したすべてのファイルを含めて 1 回で元に戻します。
  また、`/diff` は Git のないフォルダーでも使えます。
- 計画の準備ができたら、Codewhale は「確認」と「自動レビュー」のどちらで Work を始めるか、それとも計画を続けるかを尋ねます。
- `/color` は、チャットごとに入力欄の色を設定し、セッションとともに保存します。
- `web_search` で DeepSeek の新しい Search API を使えます。
  DeepSeek のキーを用意して `[search] provider = "deepseek"` を設定してください。
- フルアクセス以外では、認証情報をアップロードしてしまう `curl` や `wget` は承認なしには実行されず、不可視の Unicode 文字を隠したコマンドは拒否されます。
  `write` は、エージェントがまだ読んでいないファイルや、読んだ後に変更されたファイルを置き換えなくなりました。

### モデル

Anthropic、OpenAI、DeepSeek、Google、Mistral、Moonshot、OpenRouter、xAI など、40 を超えるプロバイダーを組み込んでいます。
OpenAI 互換のエンドポイントであればどれでも使え、Ollama、vLLM、SGLang 経由のローカルモデルも使えます。
既定は DeepSeek の `deepseek-flash` です。
DeepSeek は 2026-10-17 にこれを DeepSeek-V4.1-Flash-1017 に切り替えましたが、0.10.1 はアップデートなしで動作します（[詳細](../PROVIDERS.md#deepseek-v41-flash)）。

`/model` でターンの合間にモデルを切り替えられ、`/model auto` ではターンごとにモデルが選ばれます（選び方は `/router` で設定します）。
`/turn inspect` は、あるターンで使われたモデル、費用、所要時間を表示します。
`codewhale auth chatgpt` を使うと、API キーの代わりに ChatGPT のプランを利用できます（実験的）。
[プロバイダー](../PROVIDERS.md)を参照してください。

### レシートと取り消し

`/receipts`（終了後は `codewhale receipts --last`）は、変更されたファイル、実行されたコマンド、Web と MCP の呼び出し、起動されたエージェント、承認とその承認者、失敗を一覧表示します。
`/undo` は直前のリクエストと変更されたファイルを、信頼モードなしで一度に元に戻します。
`/restore` はワークスペース全体を以前のスナップショットまで巻き戻します。
それ以降の編集を上書きするため、`/trust on` またはフルアクセスが必要です。
[レシート](../RECEIPTS.md)を参照してください。

ツールを利用できる exec の実行は、どの出力形式でも保存されるため、`--json` を含む `exec --auto` の実行後に `codewhale receipts --last` を使えます。
回答のみを返すテキスト出力や `--json` の実行は、再開時か会話の圧縮が始まるときにだけ保存されます。
レシートや `exec --continue` のために常に保存するには、`--output-format stream-json` を使ってください。

### 長い作業

`/goal` は、ターンをまたいで持続する目標を設定します。
Codewhale は作業を[サブエージェント](../SUBAGENTS.md)に分担させたり、複数のエージェントを [Fleet](../FLEET.md) として動かしたり、リポジトリにコミットされた[ワークフロー](../WORKFLOW_AUTHORING.md)に従ったりできます。

### 他のエージェントからの移行

Codewhale は `AGENTS.md` を自動で読み込みます。
`CLAUDE.md`、`.cursorrules`、`.github/copilot-instructions.md` など、他のツール向けに書かれた指示ファイルは、設定で `project_instruction_imports = ["claude"]`（または `"cursor"`、`"cline"`、`"windsurf"`、`"copilot"`、`"all"`）を指定してオプトインした場合にのみ読み込まれます。
[config.example.toml](../../config.example.toml) を参照してください。
読み込んでいない指示ファイルを見つけると、Codewhale は警告を出します。

`/import-claude` は、Claude Code の設定と MCP サーバーのインポート内容を、確認できる形で用意します。
`/plugin install` は、スキル、コマンド、エージェントプロファイル、MCP サーバーを含む Claude Code プラグインを受け付け、フックや LSP サーバーを必要とするプラグインは拒否します（[詳細](../CLAUDE_PLUGIN_COMPAT.md)）。

### 拡張

[MCP サーバー](../MCP.md)を接続し、[スキル](../SKILLS.md)や[プラグイン](../PLUGINS.md)を追加し、セッションやツールのイベントで[フック](../HOOKS.md)を実行できます。
コードモード（実験的、既定で有効）では、モデルが複数の MCP ツールやプラグインツールを呼び出す 1 つの `execute_tools` プログラムを書けます。
個々の呼び出しは引き続き承認ルールに従い、それぞれのレシートも残ります。
個別にインストールする Computer Use プラグイン（実験的、macOS のみ）は、他のアプリを観察・操作できます。
アクセス範囲を確認して有効にするまではオフのままです（[ガイド](https://github.com/codewhale-hq/codewhale-cu-plugin)）。

## 実行方法

| コマンド | 内容 |
| --- | --- |
| `codewhale` | 対話型のターミナルアプリ |
| `codewhale exec "…"` | インターフェースなしで 1 つのタスクを実行します。オプションなしではツールを使わずに回答し、`--auto` は既定でフルアクセスでツールを有効にし、`--sandbox workspace-write` はそれらを制限し、`--output-format stream-json` でイベントを JSON としてストリーミングします |
| `codewhale web` | `127.0.0.1` で動作するローカル Web UI（[ガイド](../WEB.md)） |
| `codewhale review --pr N` | GitHub のプルリクエストに対する参考としてのレビュー。[GitHub ボット設定](../GITHUB_ACTION.md#set-up-your-own-bot) は、自動レビューと `@codewhale review` のためのバージョンを固定したワークフローを生成し、自分のモデルキーと任意の App ID を使います |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md)。スレッド、イベント、承認、各ターンのファイル変更のためのローカル HTTP API |
| `codewhale serve --acp` | stdio 経由の Agent Client Protocol。Zed などのエディター向け |

ターミナルアプリ、`exec`、`web`、Runtime API は 1 つのエンジンを共有しています（[アーキテクチャ](../ARCHITECTURE.md)）。
コミュニティが保守している [VS Code 拡張機能](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)は、サイドバーからこのエンジンに接続します（[ソース](https://github.com/HengQuWorld/CodeWhale-VSCode)）。
Runtime API の上に構築したネイティブのデスクトップアプリを開発中です。
まだ公開ダウンロードはありません。

## 料金とアカウント

Codewhale は無料で、MIT ライセンスです。
自分の API キーを使う場合は、プロバイダーに直接支払います。
ローカルモデルにキーは不要です。
価格がわかっている場合、フッターと `/cost` にセッションの費用が表示されます。
価格が不明な場合は「不明」と表示され、無料として表示されることはありません。

アカウントは必須ではありません。
`/provider` で追加したキーは、あなたのマシン上にとどまります。

## 安全性とプライバシー

「確認」と「自動レビュー」では、システムに OS のサンドボックスがあれば、コマンドはその中で実行されます。
macOS では Seatbelt、Linux では bubblewrap がインストールされていればそれを使います。
Windows にはまだ OS のサンドボックスがなく、フルアクセスではサンドボックスが無効になります。
サンドボックス内では、シェルコマンドは `~/.ssh`、`~/.aws`、`.env` ファイルなどの認証情報の保存場所を読み取れません。
0.10.2 からは、同じルールがターミナルセッションとより多くのファイルツールにも適用されます。
`/preview-request` は、次のリクエストを送信せずに、機密情報を伏せたプレビューを表示します。

テレメトリは既定で有効です。
送信するのは集計値（バージョンとプラットフォーム、セッションの長さと結果、使用した組み込みプロバイダー、機能とエラーの回数）と、90 日ごとに切り替わるランダムなインストール ID で、会話、コード、プロンプト、モデル名、ファイル名やリポジトリ名、認証情報は決して含みません。
`codewhale config set telemetry false` で無効にできます。
また、Codewhale は起動時に GitHub で新しいリリースを確認します。
これを省くには `CODEWHALE_NO_UPDATE_CHECK=1` を設定してください。
詳しくは[認可の順序](../AUTHORIZATION_ORDER.md)、[サンドボックス](../SANDBOX.md)、[テレメトリ](../TELEMETRY.md)を参照してください。

## ドキュメント

| まずはここから | リファレンス |
| --- | --- |
| [インストール](../INSTALL.md) | [設定](../CONFIGURATION.md) |
| [プロバイダーとローカルモデル](../PROVIDERS.md) | [アーキテクチャ](../ARCHITECTURE.md) |
| [モードと権限](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [キーバインド](../KEYBINDINGS.md) | [プラグイン作成](../PLUGIN_AUTHORING.md) |
| [GitHub PR レビュー](../GITHUB_ACTION.md) | [すべてのドキュメント](../README.md) |

## コミュニティ

プロバイダーが足りない、あるいはワークフローが使いにくいと感じたら、[issue を作成](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)するか、[プルリクエストを送って](../../CONTRIBUTING.md)ください。
取り込まれた作業のクレジットはコントリビューターに残ります。
どこに何があるかは[リポジトリの構成](../../CONTRIBUTING.md#project-structure)で確認できます。

[Discord](https://discord.gg/37gfS3ksug) で話すか、WeChat でメンテナーの Hunter（`hunterbown`）を追加して、Whale Brothers グループへの参加を依頼してください。

## 歴史とライセンス

Codewhale は `deepseek-tui` として始まり、今もそのプロジェクトの設定とセッションを読み込みます。
独立して保守されており、DeepSeek をはじめとするどのモデルプロバイダーとも提携していません。
[これまでに貢献してくれたすべての方](../CONTRIBUTORS.md)に感謝します。

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE)。他のオープンソースプロジェクトから改変して取り入れたコードは、[サードパーティ通知](../THIRD_PARTY_NOTICES.md)に記載しています。
