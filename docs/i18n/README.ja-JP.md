<!-- source: README.md sha256:308ec0b2a2e8 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**ターミナル向けのオープンソース Rust コーディングエージェント。あらゆるモデル、マルチエージェント Fleet、OS サンドボックス、検証可能なレシートに対応。**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[ウェブサイト](https://codewhale.net) · [ドキュメント](../README.md) · [変更履歴](../../CHANGELOG.md) · [コントリビュート](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>この翻訳は機械翻訳による下書きで、ネイティブによるレビューはまだありません。</sub>

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="ローカル Ollama (qwen3:4b) で実際のタスクを完了したターミナルの Codewhale v0.10.4：ls.py を編集し、python3 -m unittest -v test_ls.py の実行前に承認を待ち、2 件のテストに合格して結果を報告しています。">

<sub>公開版 Codewhale v0.10.4 (48669ad108fa) でローカル Ollama (qwen3:4b) を Accept Edits モードで実行した実際のセッション。</sub>

</div>

## インストール

上のインストールスクリプトは、macOS と Linux でチェックサム検証済みの `codewhale` と短縮エイリアス `codew` を `~/.local/bin` に配置します。シェルで `command not found` と表示される場合は、`~/.local/bin` を `PATH` に追加してください（[詳細](../INSTALL.md#put-it-on-your-path)）。`codewhale update` でそのままアップグレードできます。

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

ビルド済みバイナリは Linux、macOS、Windows（x64・arm64）向けに提供されます。Scoop、Nix、Linux 版 Homebrew、Android/Termux（プレビュー）、手動ダウンロードは [docs/INSTALL.md](../INSTALL.md)、コンテナイメージは [docs/DOCKER.md](../DOCKER.md) を参照してください。`PATH` 上で別バージョンのバイナリが競合しないよう、1 台につき 1 つのインストール方法のみを使用してください。

## クイックスタート

1. 作業内容をコミットまたはスタッシュし、git リポジトリのルートで `codewhale` を起動します。
2. `/provider`（または `F3`）でプロバイダーを選び、API キーを貼り付けてモデルを指定します。デフォルトは DeepSeek です。キー未設定でもローカルの Ollama サーバーが応答していれば、最初のメッセージ送信時にローカルモデルを提案します。
3. タスクを入力します：

```text
Fix the failing tests and explain what changed.
```

読み取り専用コマンドはそのまま実行されます（Ask ではすべてのコマンドの前に確認します）。変更を伴うシェルコマンドは実行前に承認を求めます（`y` で 1 回許可、`a` で同種のコマンドをセッション中許可、`n` で拒否）。`/undo` で直前のターンとファイル変更を取り消し、`/help` でコマンドとキーバインドを確認できます。

## 主な機能

- **50 以上の内蔵プロバイダーとローカルモデル：** Anthropic、OpenAI、DeepSeek（デフォルトは `deepseek-flash` / V4.1 Flash、[詳細](../PROVIDERS.md#deepseek-v41-flash)）、Google、Mistral、Moonshot、OpenRouter、xAI、任意の OpenAI 互換エンドポイント、Ollama・vLLM・SGLang などのローカルランタイムに対応。`/model` でセッション中にモデルを切り替え、`/model auto`（`/router`）でターンごとのルーティングを設定し、`/turn inspect` でレイテンシとコストを確認できます。詳しくは [プロバイダー](../PROVIDERS.md) を参照してください。
- **レシートとロールバック：** `/receipts`（終了後は `codewhale receipts --last`）で、変更ファイル、実行コマンド、Web/MCP 呼び出し、サブエージェント、承認履歴、失敗を記録します。`/undo` は直前のターンを 1 ステップで取り消し、`/restore` はワークスペースを以前のスナップショットへ戻します（`/trust on` または Full Access が必要）。詳しくは [レシート](../RECEIPTS.md) を参照してください。
- **サブエージェント、Fleet、ワークフロー：** `/goal` で複数ターンにわたる目標を保持し、[サブエージェント](../SUBAGENTS.md) への委任、役割とモデルを分けた [Fleet](../FLEET.md) の協調実行、リポジトリ管理の [ワークフロー](../WORKFLOW_AUTHORING.md) 実行が行えます。
- **MCP、スキル、フック、プラグイン：** [MCP サーバー](../MCP.md)、[スキル](../SKILLS.md)、[プラグイン](../PLUGINS.md)、ライフサイクル [フック](../HOOKS.md) を接続できます。実験的なコードモード（`execute_tools`）は、通常の承認ルールの下で複数の MCP/プラグイン呼び出しをまとめて実行します。オプションの macOS 向け [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) は、アクセス権限を確認して有効化するまでオフのままです。
- **既存ツールとの互換性：** `AGENTS.md` を自動で読み込みます。`CLAUDE.md`、`.cursorrules`、`.github/copilot-instructions.md` は [config.example.toml](../../config.example.toml) の `project_instruction_imports` で明示的に有効化できます。`/import-claude` で Claude Code の設定と MCP サーバーを取り込み、`/plugin install` で Claude Code プラグイン（スキル、コマンド、エージェントプロファイル、MCP）および DeepSeek Harness (DSH) プラグインを導入できます（[詳細](../CLAUDE_PLUGIN_COMPAT.md)）。

## 起動方法

| コマンド | 用途 |
| --- | --- |
| `codewhale` | 対話型ターミナルアプリ |
| `codewhale exec "…"` | スクリプトや CI 向けのヘッドレス単発実行（[ガイド](../INSTALL.md#headless-scripts-ci)；サンドボックス未指定の `--auto` は Full Access で動作） |
| `codewhale web` | `127.0.0.1` で動作するローカル Web クライアント（[ガイド](../WEB.md)） |
| `codewhale review --pr N` | GitHub PR のアドバイザリーレビュー；[GitHub Bot セットアップ](../GITHUB_ACTION.md#set-up-your-own-bot) で自分のキーを使って実行 |
| `codewhale app-server --http` | スレッド、イベント、承認、ターンごとの差分を扱うローカル HTTP [Runtime API](../RUNTIME_API.md) |
| `codewhale serve --acp` | Zed などのエディタ向けに stdio で提供する Agent Client Protocol |

すべての入口は単一の Rust エンジンを共有しています（[アーキテクチャ](../ARCHITECTURE.md)）。コミュニティ保守の [VS Code 拡張機能](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)（[ソース](https://github.com/HengQuWorld/CodeWhale-VSCode)）も同じエンジンに接続します。現在のリリース状況やデスクトップ・クラウドの進捗は [最新リリースノート](https://github.com/codewhale-hq/Codewhale/releases/latest) と [ロードマップ](../ROADMAP.md) を参照してください。

## 権限、サンドボックス、プライバシー

`Shift+Tab` で権限モードを切り替えます：**Ask → Accept Edits → Plan → Auto-Review → Full Access**（`Tab` は補完のみ）。**Accept Edits**（デフォルト）は git リポジトリ内のファイル編集を自動適用し、コマンド実行前に確認します。**Ask** はすべての編集とコマンドで確認します。**Plan** は読み取り専用です。**Auto-Review** はルールとレビューモデルで通常の呼び出しを許可し、高リスクな操作を拒否します。**Full Access** は確認を省略し OS サンドボックスを無効化します（リポジトリルールと管理ポリシーは引き続き適用されます）。詳細は [モード](../MODES.md) を参照してください。

Ask、Accept Edits、Auto-Review では、対応 OS のサンドボックス内でコマンドを実行します（macOS は Seatbelt、Linux は bubblewrap、Windows は OS サンドボックス未対応）。サンドボックス内のコマンドとターミナルセッションは、`~/.ssh`、`~/.aws`、`.env` などの認証情報パスを読み取れません。`/preview-request` を使うと、送信前にマスク済みのリクエスト内容を確認できます。

`/provider` で追加した API キーはローカルマシンにのみ保存され、ローカルモデルにはキー不要です。`/cost` とフッターは価格が判明しているセッション費用を表示します（価格不明の場合は無料とせず不明と表示します）。

集計テレメトリはデフォルトで有効です（バージョン、OS、セッション長と結果、内蔵プロバイダー ID、内蔵公開カタログのモデル ID（カスタム・プライベート・ローカルモデルは `other` に分類）、トークン合計、内蔵ツールの呼び出し・エラー回数、機能・エラー回数；90 日ごとにローテーションするランダムなインストール ID；コード・プロンプト・会話・カスタム／プライベートモデル名・ファイル／リポジトリ／ブランチ名・認証情報・IP アドレスは一切送信しません）。無効化するには次を実行します：

```bash
codewhale config set telemetry false
```

起動時には GitHub の新リリース確認（`CODEWHALE_NO_UPDATE_CHECK=1` でスキップ）と、24 時間ごとの公開 `models.dev` モデルカタログ更新も行います（`CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` または `[catalog] auto_refresh = false` でスキップ、CI 環境と `DO_NOT_TRACK` では自動抑止）。詳細は [認可順序](../AUTHORIZATION_ORDER.md)、[サンドボックス](../SANDBOX.md)、[テレメトリ](../TELEMETRY.md) を参照してください。

## ドキュメント

ガイドは `main` ブランチに追従しているため、最新リリース未収録の変更を含む場合があります。

| はじめに | リファレンス |
| --- | --- |
| [ユーザーガイド](../GUIDE.md) | [設定](../CONFIGURATION.md) |
| [インストール](../INSTALL.md) | [アーキテクチャ](../ARCHITECTURE.md) |
| [プロバイダーとローカルモデル](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [モードと権限](../MODES.md) | [プラグイン開発](../PLUGIN_AUTHORING.md) |
| [キーバインド](../KEYBINDINGS.md) | [レシート](../RECEIPTS.md) |
| [GitHub PR レビュー](../GITHUB_ACTION.md) | [全ドキュメント](../README.md) |

## コントリビュートとライセンス

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale コントリビューター一覧"></a>

[コントリビューター一覧](../CONTRIBUTORS.md)（`scripts/contributor-wall.py`）には、コード提供、共同コミット、引き継ぎ PR、報告者が記録されます。バグ報告や機能提案は [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose)、質問は [Discussions](https://github.com/codewhale-hq/Codewhale/discussions)、PR ガイドラインと構成は [CONTRIBUTING.md](../../CONTRIBUTING.md)（[プロジェクト構成](../../CONTRIBUTING.md#project-structure)）をご覧ください。[Discord](https://discord.gg/37gfS3ksug) に参加するか、WeChat で `hunterbown` を追加して Whale Brothers グループへどうぞ。

Codewhale は `deepseek-tui` から派生しており、その設定とセッションとの後方互換性を維持しています。独立して保守されており、DeepSeek やその他のモデルプロバイダーとは提携していません。

[MIT](../../LICENSE) ライセンスです。サードパーティ表記は [同梱ソース](../../THIRD_PARTY_NOTICES.md) および [参考設計](../THIRD_PARTY_NOTICES.md) に記載しています。
