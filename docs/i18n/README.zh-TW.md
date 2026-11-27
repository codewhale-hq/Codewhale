<!-- source: README.md sha256:308ec0b2a2e8 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**開源 Rust 終端機程式設計代理。支援任何模型、多代理編隊、作業系統沙盒與可驗證收據。**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[網站](https://codewhale.net) · [文件](../README.md) · [更新紀錄](../../CHANGELOG.md) · [參與貢獻](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>本翻譯為機器草稿，尚未經母語者審校。</sub>

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 在終端機中以本機 Ollama (qwen3:4b) 完成真實任務後的畫面：它編輯了 ls.py，在執行 python3 -m unittest -v test_ls.py 前等待核准，2 項測試全部通過並回報結果。">

<sub>在已發布的 Codewhale v0.10.4 (48669ad108fa) 上使用本機 Ollama (qwen3:4b) 以 Accept Edits 模式執行的真實工作階段。</sub>

</div>

## 安裝

上方安裝腳本會在 macOS 與 Linux 上將經過校驗碼驗證的 `codewhale` 及其短別名 `codew` 安裝至 `~/.local/bin`。若終端機顯示 `command not found`，請將 `~/.local/bin` 加入 `PATH`（[詳情](../INSTALL.md#put-it-on-your-path)）。執行 `codewhale update` 可原地升級。

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

預編譯執行檔涵蓋 Linux、macOS 與 Windows（x64 與 arm64）。Scoop、Nix、Linux Homebrew、Android/Termux（預覽版）與手動下載方式請見 [docs/INSTALL.md](../INSTALL.md)，容器映像請見 [docs/DOCKER.md](../DOCKER.md)。每台機器請僅使用一種安裝方式，避免 `PATH` 上出現彼此遮蔽的執行檔。

## 快速開始

1. 先提交或暫存目前修改，接著在 git 儲存庫根目錄執行 `codewhale`。
2. 執行 `/provider`（或按 `F3`）選擇模型供應商、貼上 API 金鑰並挑選模型。預設為 DeepSeek；若未設定金鑰且本機 Ollama 伺服器有回應，Codewhale 會在你送出第一則訊息時提供其模型。
3. 輸入任務：

```text
Fix the failing tests and explain what changed.
```

唯讀指令會立即執行（Ask 模式下每個指令都會先詢問）。會變更狀態的 Shell 指令會先請求核准：`y` 允許執行一次，`a` 允許本工作階段中同類指令，`n` 拒絕。執行 `/undo` 可復原上一輪操作與檔案修改，執行 `/help` 可查看指令與快捷鍵。

## 核心能力

- **50+ 內建供應商與本機模型：** 支援 Anthropic、OpenAI、DeepSeek（預設 `deepseek-flash` / V4.1 Flash，[詳情](../PROVIDERS.md#deepseek-v41-flash)）、Google、Mistral、Moonshot、OpenRouter、xAI、任何 OpenAI 相容端點，以及 Ollama、vLLM 或 SGLang 等本機執行環境。使用 `/model` 隨時切換模型，`/model auto`（`/router`）依每輪自動路由，`/turn inspect` 檢視延遲與費用。詳見 [供應商文件](../PROVIDERS.md)。
- **收據與復原：** `/receipts`（或離開後執行 `codewhale receipts --last`）記錄修改的檔案、執行的指令、Web/MCP 呼叫、子代理、核准紀錄與失敗項目。`/undo` 一步還原上一輪修改；`/restore` 將工作區回溯至較早快照（需 `/trust on` 或 Full Access）。詳見 [收據文件](../RECEIPTS.md)。
- **子代理、編隊與工作流：** 以 `/goal` 跨輪次維持目標，將工作委派給 [子代理](../SUBAGENTS.md)，協調依角色與模型分工的 [Fleet 編隊](../FLEET.md)，或執行儲存庫內建的 [工作流](../WORKFLOW_AUTHORING.md)。
- **MCP、技能、掛鉤與外掛：** 連接 [MCP 伺服器](../MCP.md)，載入 [技能](../SKILLS.md)、[外掛](../PLUGINS.md) 與生命週期 [掛鉤](../HOOKS.md)。實驗性程式碼模式（`execute_tools`）可在一般核准規則下批次呼叫多個 MCP/外掛工具。選用的 macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) 預設關閉，需經你審查權限後手動啟用。
- **無縫相容：** 自動讀取 `AGENTS.md`。如需載入 `CLAUDE.md`、`.cursorrules` 或 `.github/copilot-instructions.md`，可在 [config.example.toml](../../config.example.toml) 中設定 `project_instruction_imports`。透過 `/import-claude` 匯入 Claude Code 設定與 MCP 伺服器，或使用 `/plugin install` 安裝 Claude Code 外掛（技能、指令、代理設定檔、MCP）與 DeepSeek Harness (DSH) 外掛（[詳情](../CLAUDE_PLUGIN_COMPAT.md)）。

## 執行入口

| 指令 | 用途 |
| --- | --- |
| `codewhale` | 互動式終端機應用程式 |
| `codewhale exec "…"` | 適合腳本與 CI 的無介面單任務執行器（[指南](../INSTALL.md#headless-scripts-ci)；未啟用沙盒時 `--auto` 使用 Full Access） |
| `codewhale web` | 執行於 `127.0.0.1` 的本機 Web 用戶端（[指南](../WEB.md)） |
| `codewhale review --pr N` | 建議性 GitHub PR 審查；[GitHub 機器人設定](../GITHUB_ACTION.md#set-up-your-own-bot) 使用你自己的金鑰執行 |
| `codewhale app-server --http` | 本機 HTTP [Runtime API](../RUNTIME_API.md)，提供執行緒、事件、核准與每輪 diff |
| `codewhale serve --acp` | 透過 stdio 提供 Agent Client Protocol，支援 Zed 等編輯器 |

所有入口共用同一個 Rust 引擎（[架構說明](../ARCHITECTURE.md)）。社群維護的 [VS Code 擴充功能](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)（[原始碼](https://github.com/HengQuWorld/CodeWhale-VSCode)）同樣連線至此引擎。最新發行與桌面端／雲端進度請見 [最新發行說明](https://github.com/codewhale-hq/Codewhale/releases/latest) 與 [路線圖](../ROADMAP.md)。

## 權限、沙盒與隱私

按 `Shift+Tab` 切換權限模式：**Ask → Accept Edits → Plan → Auto-Review → Full Access**（`Tab` 僅用於自動補全）。**Accept Edits**（預設）直接套用 git 儲存庫內的檔案編輯，並在執行指令前詢問。**Ask** 於每次編輯與指令前詢問。**Plan** 為唯讀模式。**Auto-Review** 透過規則與審查模型核准例行呼叫並阻擋高風險操作。**Full Access** 略過提示並停用作業系統沙盒（儲存庫規則與受管原則仍適用）。詳見 [權限模式](../MODES.md)。

在 Ask、Accept Edits 與 Auto-Review 模式下，指令會在可用的作業系統沙盒內執行：macOS 使用 Seatbelt，Linux 使用 bubblewrap（Windows 尚無 OS 沙盒）。沙盒內的指令與終端機工作階段無法讀取 `~/.ssh`、`~/.aws` 或 `.env` 等憑證路徑。執行 `/preview-request` 可在送出前檢視去識別化的請求預覽。

透過 `/provider` 新增的 API 金鑰只保留在本機，本機模型不需金鑰。`/cost` 與頁尾會在已知價格時追蹤工作階段費用（未知價格會顯示為未知，絕不顯示為免費）。

彙總遙測預設啟用（版本、作業系統、工作階段長度與結果、內建供應商 ID、內建公開目錄中的模型 ID（自訂、私有或本機模型記為 `other`）、彙總 token 數量、內建工具呼叫與錯誤計數、功能與錯誤計數；每 90 天輪換的隨機安裝 ID；絕不上傳程式碼、提示詞、對話內容、自訂/私有模型名稱、檔案/儲存庫/分支名稱、憑證或 IP 位址）。執行以下指令即可關閉：

```bash
codewhale config set telemetry false
```

啟動時 Codewhale 也會檢查 GitHub 是否有新版本（設定 `CODEWHALE_NO_UPDATE_CHECK=1` 可略過），並每 24 小時重新整理一次公開的 `models.dev` 模型目錄（設定 `CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` 或 `[catalog] auto_refresh = false` 可略過；在 CI 環境與 `DO_NOT_TRACK` 下自動停用）。詳見 [授權順序](../AUTHORIZATION_ORDER.md)、[沙盒機制](../SANDBOX.md) 與 [遙測說明](../TELEMETRY.md)。

## 文件

以下指南跟隨 `main` 分支更新，可能包含尚未進入最新發行版的變更。

| 快速入門 | 參考文件 |
| --- | --- |
| [使用者指南](../GUIDE.md) | [設定說明](../CONFIGURATION.md) |
| [安裝指南](../INSTALL.md) | [架構設計](../ARCHITECTURE.md) |
| [供應商與本機模型](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [模式與權限](../MODES.md) | [外掛開發](../PLUGIN_AUTHORING.md) |
| [快捷鍵](../KEYBINDINGS.md) | [收據說明](../RECEIPTS.md) |
| [GitHub PR 審查](../GITHUB_ACTION.md) | [所有文件](../README.md) |

## 貢獻與授權條款

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale 貢獻者牆"></a>

[貢獻者牆](../CONTRIBUTORS.md)（`scripts/contributor-wall.py`）記錄所有程式碼提交、共同作者、延續併入的 PR 與署名回報。錯誤回報與功能建議請至 [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose)，問題討論請至 [Discussions](https://github.com/codewhale-hq/Codewhale/discussions)，PR 規範與目錄架構請見 [CONTRIBUTING.md](../../CONTRIBUTING.md)（[專案結構](../../CONTRIBUTING.md#project-structure)）。歡迎加入 [Discord](https://discord.gg/37gfS3ksug)，或在 WeChat 加入 `hunterbown` 申請加入 Whale Brothers 群組。

Codewhale 源自 `deepseek-tui`，並維持對其設定與工作階段的向下相容。本專案由社群獨立維護，與 DeepSeek 或任何其他模型供應商無隸屬關係。

採用 [MIT](../../LICENSE) 授權條款。第三方聲明請見 [內嵌原始碼聲明](../../THIRD_PARTY_NOTICES.md) 與 [改編設計聲明](../THIRD_PARTY_NOTICES.md)。
