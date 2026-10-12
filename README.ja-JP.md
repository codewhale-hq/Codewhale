<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**どのモデルでも使える、オープンソースのコーディングエージェント。**

Codewhale はプロジェクトを読み、ファイルを編集し、コマンドを実行して、自分の作業を確認します。
ターミナル上で、あなたが選んだホスト型またはローカルのモデルを使います。

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[ウェブサイト](https://codewhale.net) · [ドキュメント](docs/README.md) · [変更履歴](CHANGELOG.md) · [コントリビュート](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Codewhale のターミナルセッション" width="760">

<sub>設定済みのローカルデモセッションから取得した、実際の Codewhale ターミナル画面です。</sub>

</div>

## インストール

macOS と Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

インストーラーはチェックサム検証済みのバイナリを `~/.local/bin` にダウンロードします。
その後 `codewhale` が "command not found" と表示される場合は、インストーラーが出力する PATH 用の 1 行を実行するか、
[PATH に追加する](docs/INSTALL.md#put-it-on-your-path)を参照してください。
アップグレードはいつでも `codewhale update` で行えます。

<details>
<summary><b>Windows、npm、Cargo、その他のインストール方法</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker、Nix、Linux 上の Homebrew、Android/Termux、チェックサム検証付きの手動ダウンロード、
任意の CNB ミラーについては[インストールガイド](docs/INSTALL.md)で説明しています。
方法は 1 つだけ選んでください。1 台のマシンに複数の方法でインストールすると、`PATH` の中で競合します。

</details>

## クイックスタート

**プロバイダーの API キーを一か所で管理。** [Codewhale アカウントを作成](https://app.codewhale.net/register)するか、
`codewhale login` を実行してログインしてください。API キーをアカウントに保存・更新すると、
ログイン済みのデバイスから Codewhale のモデルルート経由で利用できます。
[アカウントのキーを設定する](docs/CONFIGURATION.md#account-provider-keys)。
ログインしても既存のローカルキーはアップロードされません。アカウントなしでもローカルで利用できます。

1. **プロジェクトを開く。** 作業したいフォルダーで `codewhale` を実行します。
2. **モデルを接続する。** `/provider`（または `F3`）を実行して、ホスト型のキーを追加するか、ローカルランタイムを選びます。
   チャットモデルを載せた Ollama がすでに起動していれば、Codewhale は自動的にそれへ切り替わります。モデルの変更は `/model` で行います。
3. **具体的なタスクを渡す。**

```text
Fix the failing tests and explain what changed.
```

同じタスクを、スクリプトや CI ジョブからヘッドレスで実行することもできます。

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

コマンドとキーボードショートカットは `/help` で確認できます。

## 実行方法

どのクライアントも同じローカルの Codewhale Runtime を操作するため、セッション、ツール、権限はどこでも同じように動作します。

| コマンド | 内容 |
| --- | --- |
| `codewhale` | 対話型のターミナルインターフェース |
| `codewhale exec "…"` | スクリプトや CI からヘッドレスで 1 ターン実行します。`--auto` でファイルおよびシェルツールを許可し、`--output-format stream-json` で JSON をストリーミング出力します |
| `codewhale web` | `127.0.0.1` で動作する、同梱の[ローカルブラウザークライアント](docs/WEB.md) |
| `codewhale review --pr N` | 参考情報としての[プルリクエストレビュー](docs/GITHUB_ACTION.md)。投稿は任意で有効化 |
| Runtime API | スレッド、イベント、承認のための[ローカル HTTP API](docs/RUNTIME_API.md) |

ネイティブのデスクトップアプリ（GPUI）は、サインインして使う製品クライアントとして開発中です。提供状況は
[製品ページ](https://codewhale.net/en/product)をご覧ください。コミュニティが保守している
[VS Code 拡張機能](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)は、
サイドバーから同じ Runtime に接続します（[ソース](https://github.com/HengQuWorld/CodeWhale-VSCode)）。

## 主な機能

- **どのモデルでも、ロックインなし。** Anthropic、DeepSeek、Google、Mistral、Moonshot、OpenAI、OpenRouter、xAI など、
  40 以上のプロバイダー経路を内蔵。OpenAI 互換のエンドポイントや、Ollama、vLLM、SGLang 経由のローカルモデルも使えます。
  [プロバイダー](docs/PROVIDERS.md)
- **主導権はあなたに。** Plan モードは何も変更せずに調査だけを行い、Work と Operate は変更を加えます。
  承認ポスチャーがツール呼び出しにあなたの許可を求めるタイミングを決め、`/undo` と `/restore` でワークスペースの変更を元に戻せます。
  `/receipts` はセッション内のすべてのファイル、コマンド、承認を一覧表示します。[モード](docs/MODES.md) ·
  [レシート](docs/RECEIPTS.md)
- **長い作業のために。** 持続する `/goal` を設定し、範囲を区切った作業を
  [サブエージェント](docs/SUBAGENTS.md)に任せ、支出の事前チェック付きで監督される
  [エージェントチーム](docs/FLEET.md)を動かし、あるいはリポジトリに含められる
  [ワークフロー](docs/WORKFLOW_AUTHORING.md)としてスクリプト化できます。
- **普段使っているものを拡張。** [MCP サーバー](docs/MCP.md)を接続し、
  [スキル](docs/SKILLS.md)と[プラグイン](docs/PLUGINS.md)をインストールし、
  セッションやツールのイベントで[フック](docs/HOOKS.md)を実行し、既存の
  [Claude Code プラグイン](docs/CLAUDE_PLUGIN_COMPAT.md)も読み込めます。
- **Computer Use。** 同梱のプラグインが、他のアプリケーションを観察・操作するツールを追加します。
  使用前にアクセス範囲を確認し、有効化してください。
  [ガイド](crates/tui/plugins/computer-use/README.md)

## モードと権限

| | 切り替え方法 | 選択肢 |
| --- | --- | --- |
| **モード（Mode）**: エージェントが何をしているか | `Tab` または `/mode` | Plan（調査のみ、変更なし）· Work（編集と実行）· Operate（計画され検証されたステップで目標を進める） |
| **ポスチャー（Posture）**: いつ先に確認するか | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access でも、ハードなポリシー境界は守られます。
各選択肢については[モードと権限のガイド](docs/MODES.md)で説明しています。

## 安全性

Codewhale はあなたのマシン上で、あなたが与えたアクセス権の範囲で動作します。承認ポスチャーとリポジトリのルールがエージェントにできることを制限し、
対応環境ではコマンドは OS のサンドボックス内で実行されます（macOS は Seatbelt、Linux では bubblewrap がインストールされ正常に動作する場合）。
`/preview-request` は、何かを送信する前に、機密情報を伏せたリクエストそのものを表示します。
価格が不明なモデルは「不明」のまま扱われ、無料として報告されることはありません。

[認可の順序](docs/AUTHORIZATION_ORDER.md)、
[サンドボックス](docs/SANDBOX.md)、[テレメトリ](docs/TELEMETRY.md)を参照してください。
利用回数の集計は既定で有効で、`codewhale config set telemetry false` で無効にできます。

## ドキュメント

| まずはここから | さらに詳しく |
| --- | --- |
| [インストール](docs/INSTALL.md) | [設定](docs/CONFIGURATION.md) |
| [プロバイダーとローカルモデル](docs/PROVIDERS.md) | [アーキテクチャ](docs/ARCHITECTURE.md) |
| [モードと権限](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [キーバインド](docs/KEYBINDINGS.md) | [プラグイン作成](docs/PLUGIN_AUTHORING.md) |
| [GitHub PR レビュー](docs/GITHUB_ACTION.md) | [すべてのドキュメント](docs/README.md) |

## コミュニティ

バグ報告、機能のアイデア、プルリクエストを歓迎します。Codewhale を何か月も使っている方も、初めて試す方も同じです。
プロバイダーが足りない、あるいはワークフローが使いにくいと感じたら、
[issue を作成](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)するか、
[プルリクエストを送って](CONTRIBUTING.md)ください。初めてのコントリビュートも歓迎で、
取り込まれた作業の貢献者としてのクレジットは保たれます。
[リポジトリの構成](CONTRIBUTING.md#project-structure)が最初の手がかりになります。

[Discord](https://discord.gg/37gfS3ksug) に参加するか、WeChat で Hunter
（`hunterbown`）を追加して、Whale Brothers グループへの参加を依頼してください。

## 歴史とライセンス

Codewhale は `deepseek-tui` として始まり、今もそのプロジェクトの設定とセッションを読み込みます。
現在は特定のプロバイダーに依存せず、独立して保守されており、どのモデルプロバイダーとも提携していません。
[すべてのコントリビューター](docs/CONTRIBUTORS.md)と、成長を支えてくれたオープンソースコミュニティに感謝します。

[MIT](LICENSE)。他のオープンソースプロジェクトから改変して取り入れた部分は、
[サードパーティ通知](docs/THIRD_PARTY_NOTICES.md)に記載しています。
