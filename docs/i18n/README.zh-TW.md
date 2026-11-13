<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>模型可以是任何人的。代理應該屬於你。</h1>

Codewhale 是一個在終端機中執行的開源程式設計代理，以 Rust 撰寫。它支援任何模型，可以把一項任務分給多個代理，而且它的每一行程式碼都任你閱讀。

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

這會在 macOS 或 Linux 上安裝最新發行版本，不需要帳號。[Windows、npm 和 Cargo](#安裝) 的安裝方式見下文。

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[網站](https://codewhale.net) · [文件](../README.md) · [更新紀錄](../../CHANGELOG.md) · [參與貢獻](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>本翻譯為機器草稿，尚未經母語者審校。</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="一個示範任務結束後的 Codewhale 終端機畫面。它回報已為 export.py 加上 --json 參數，3 個本機測試全部通過，委派的審查沒有發現問題。下方的任務清單顯示三個已完成的步驟，狀態列顯示 Full Access 和本機 website-demo 提供者。">

<sub>尚未發布的 0.10.3 開發版建置，使用腳本化的測試模型，在一次性工作區中以 Full Access 執行。其中的編輯、三個測試和一次子代理審查都是真實執行的。</sub>

</div>

## 安裝

上面的指令會把經過總和檢查碼驗證的 `codewhale` 及其簡短別名 `codew` 放到 `~/.local/bin`。如果之後 shell 顯示 `command not found`，請依照安裝程式印出的 PATH 提示操作（[詳情](../INSTALL.md#put-it-on-your-path)）。使用 `codewhale update` 升級。

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

預先編譯的二進位檔涵蓋 Linux、macOS 和 Windows 的 x64 與 arm64 架構。Scoop、Nix、Linux 上的 Homebrew、Android 和 Termux（預覽版）以及手動下載請見[安裝指南](../INSTALL.md)，容器請見 [Docker](../DOCKER.md)。每台電腦只使用一種安裝方式；否則 `PATH` 上會出現兩份副本，排在前面的那一份生效。

## 快速開始

1. 提交或暫存你的變更，然後在 git 儲存庫根目錄執行 `codewhale`。預設情況下，它會直接套用儲存庫內的檔案修改而不詢問，並在每個非唯讀的 shell 指令前先詢問你。
2. 執行 `/provider`（或按 `F3`），選擇一個提供者，貼上它的 API 金鑰，然後選擇模型。預設提供者是 DeepSeek。首次啟動且沒有金鑰時，Codewhale 會在你送出第一則訊息時請你選擇提供者；如果本機 Ollama 伺服器有回應，它會提議使用本機模型。
3. 輸入一個任務：

```text
Fix the failing tests and explain what changed.
```

唯讀指令會直接執行。其他任何 shell 指令，Codewhale 都會先詢問：`y` 執行這一次，`a` 在本次對話接下來的時間裡允許類似的指令，`n` 拒絕。`/undo` 會復原你的上一個請求及其檔案變更，`/help` 列出指令和按鍵。

在腳本和 CI 中，`codewhale exec` 不啟動介面，只執行一個任務。用 `--auto` 給它工具之前，請先閱讀[無介面執行](../INSTALL.md#headless-scripts-ci)，因為除非你加上沙箱，`--auto` 使用的是 Full Access。

## 功能

給 Codewhale 一個任務，它會閱讀儲存庫、編輯檔案並執行指令，直到任務完成或需要你的輸入。[最新版本的發行說明](https://github.com/codewhale-hq/Codewhale/releases/latest)列出最新變更，[更新日誌](../../CHANGELOG.md)包含每個版本。

### 任何模型

內建超過 40 個提供者，包括 Anthropic、OpenAI、DeepSeek、Google、Mistral、Moonshot、OpenRouter 和 xAI。任何相容 OpenAI 的端點都可以使用，透過 Ollama、vLLM 或 SGLang 執行的本機模型也可以。預設模型是 DeepSeek 的 `deepseek-flash`，它現在由 DeepSeek V4.1 Flash 提供（[詳情](../PROVIDERS.md#deepseek-v41-flash)）。

`/model` 可在回合之間切換模型，`/model auto` 會為每個回合挑選一個模型（用 `/router` 設定挑選方式）。`/turn inspect` 顯示某個回合使用了哪個模型、花費多少以及耗時多久。請見[提供者](../PROVIDERS.md)。

### 收據與復原

`/receipts`（離開後可用 `codewhale receipts --last`）會列出一次工作階段修改過的檔案、執行過的指令、發出的網頁和 MCP 呼叫、啟動的代理、每次核准及核准者，以及失敗的操作。`/undo` 一步復原你的上一個請求及其修改的檔案。`/restore` 會把整個工作區回復到較早的快照；由於這會覆寫之後的編輯，它需要 `/trust on` 或 Full Access。請見[收據](../RECEIPTS.md)。

### 多個代理共同完成一項任務

`/goal` 設定一個跨回合延續的目標。Codewhale 可以把工作分給[子代理](../SUBAGENTS.md)，執行一組各有角色和模型的代理 [Fleet](../FLEET.md)，或依照提交在儲存庫中的[工作流程](../WORKFLOW_AUTHORING.md)執行。

### 擴充

連接 [MCP 伺服器](../MCP.md)，加入[技能](../SKILLS.md)和[外掛](../PLUGINS.md)，並在工作階段和工具事件上執行[掛鉤](../HOOKS.md)。程式碼模式（實驗性，預設開啟）讓模型撰寫一個呼叫多個 MCP 和外掛工具的 `execute_tools` 程式；每次呼叫仍須遵守你的核准規則，並保留收據。v0.10.2 內建的 Computer Use 外掛（實驗性，僅限 macOS）可以觀察並操作其他應用程式，在你檢查其權限並啟用之前一直保持關閉（[指南](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)）。即將在 0.10.3 推出：此外掛將[另外發行](https://github.com/codewhale-hq/codewhale-cu-plugin)。

### 執行方式

| 指令 | 用途 |
| --- | --- |
| `codewhale` | 互動式終端機應用程式 |
| `codewhale exec "…"` | 不啟動介面執行一個任務，用於腳本和 CI（[無介面執行](../INSTALL.md#headless-scripts-ci)） |
| `codewhale web` | 在 `127.0.0.1` 上執行的本機網頁用戶端（[指南](../WEB.md)） |
| `codewhale review --pr N` | 對 GitHub 提取要求進行建議性審查；[GitHub 機器人設定](../GITHUB_ACTION.md#set-up-your-own-bot)以你自己的模型金鑰執行它 |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md)，一個本機 HTTP API，提供執行緒、事件、核准以及每個回合的檔案變更 |
| `codewhale serve --acp` | 透過 stdio 提供 Agent Client Protocol，供 Zed 等編輯器使用 |

終端機應用程式、`exec`、`web` 和 Runtime API 共用同一個引擎（[架構](../ARCHITECTURE.md)）。社群維護的 [VS Code 擴充功能](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)可以從側邊欄連接到它（[原始碼](https://github.com/HengQuWorld/CodeWhale-VSCode)）。

## 如何保持安全

兩個開關決定 Codewhale 可以自行做多少事。輸入框為空時，`Tab` 切換模式：**Plan** 只閱讀和搜尋，不做任何修改；**Work** 編輯檔案並執行指令；**Operate** 與 Work 權限相同，會持續工作到目標通過驗證或需要你介入。`Shift+Tab` 切換權限。預設的 **Ask** 會直接套用儲存庫內的檔案修改而不詢問，並在非唯讀的 shell 指令前詢問。**Auto-Review** 讓規則和一個審查模型核准例行呼叫，並拒絕高風險呼叫；切換到 Ask 即可自行核准這些呼叫。**Full Access** 從不詢問，並關閉 Codewhale 的沙箱，但儲存庫規則和受管政策仍然適用。[v0.10.2 指南](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md)有詳細說明。

即將在 0.10.3 推出：`Shift+Tab` 將成為依序經過 Ask、Accept Edits、Plan、Auto-Review 和 Full Access 的轉盤，`Tab` 不再切換模式，Operate 也將取消。Ask 將在每次檔案修改前也詢問，而新的預設選項 Accept Edits 會套用 git 儲存庫內的修改，並仍在指令前詢問。[目前的指南](../MODES.md)有說明。

在 Ask 和 Auto-Review 中，指令會在系統提供的作業系統沙箱中執行：macOS 上是 Seatbelt，Linux 上安裝了 bubblewrap 時是 bubblewrap。Windows 目前還沒有作業系統沙箱，Full Access 會關閉沙箱。在沙箱內，shell 指令和終端機工作階段無法讀取 `~/.ssh`、`~/.aws` 和 `.env` 檔案等憑證儲存。`/preview-request` 會顯示下一個請求的遮蔽後預覽，而不會實際送出。

你不需要帳號，用 `/provider` 加入的金鑰只保存在你的電腦上。使用你自己的 API 金鑰時，你直接向提供者付費；本機模型不需要金鑰。價格已知時，頁尾和 `/cost` 會顯示一次工作階段的花費；價格未知時顯示為未知，絕不顯示為免費。

遙測預設開啟。它會傳送彙總計數（版本和平台、工作階段時長和結果、你用過的內建提供者、功能和錯誤計數），附帶一個每 90 天輪替的隨機安裝 ID，絕不傳送對話、程式碼、提示詞、模型名稱、檔案或儲存庫名稱以及憑證。用這個指令關閉它：

```bash
codewhale config set telemetry false
```

Codewhale 啟動時也會在 GitHub 上檢查新版本；設定 `CODEWHALE_NO_UPDATE_CHECK=1` 可以略過。更多內容請見[授權順序](../AUTHORIZATION_ORDER.md)、[沙箱](../SANDBOX.md)和[遙測](../TELEMETRY.md)。

## 未來方向

Codewhale 正在成為橫跨你的終端機、桌面應用程式、瀏覽器和雲端電腦的統一環境，全部由同一個引擎驅動。下面每個部分都說明了它今天所處的狀態。

| 部分 | 狀態 | 現況 |
| --- | --- | --- |
| 終端機應用程式 | 已發行 | 在 macOS、Linux 或 Windows 上安裝，然後在任何儲存庫中執行 `codewhale`。 |
| 本機瀏覽器用戶端 | 已發行 | 執行 `codewhale web`，即可在瀏覽器中閱讀對話、送出任務並回應核准；它隨終端機一起提供，不需要帳號。 |
| 多個代理共同完成一項任務 | 已發行 | 子代理、Fleet 和工作流程讓多個代理各司其職，共同完成一項任務。 |
| 桌面應用程式 | 開發中，尚無下載 | 我們正在以同一個引擎打造桌面應用程式，目前還沒有公開下載。 |
| 雲端電腦 | 僅限邀請 | 雲端電腦僅限邀請，我們正在打造它們，讓工作階段可以在雲端的一台機器上持續工作。 |
| 貫穿它們的同一個工作階段 | 建置中 | 我們正在打造同一個工作階段，它會隨你從終端機到桌面、瀏覽器和雲端，並且自始至終都屬於你。 |

[路線圖](../ROADMAP.md)說明了每個版本涵蓋的內容，以及你可以在哪裡提供協助。

## 從其他代理轉換過來

Codewhale 會自動讀取 `AGENTS.md`。為其他工具撰寫的指示檔案，例如 `CLAUDE.md`、`.cursorrules` 或 `.github/copilot-instructions.md`，只有在你於設定中加入 `project_instruction_imports = ["claude"]`（或 `"cursor"`、`"cline"`、`"windsurf"`、`"copilot"`、`"all"`）選擇啟用後才會載入；請見 [config.example.toml](../../config.example.toml)。Codewhale 發現未載入的此類檔案時會提出警告。

`/import-claude` 會準備一份 Claude Code 設定和 MCP 伺服器的匯入內容，供你審閱。`/plugin install` 接受包含技能、指令、代理設定檔和 MCP 伺服器的 Claude Code 外掛，並拒絕需要掛鉤或 LSP 伺服器的外掛（[詳情](../CLAUDE_PLUGIN_COMPAT.md)）。

## 文件

這些指南跟隨主分支，可能會描述尚未包含在最新版本中的變更。

| 從這裡開始 | 參考 |
| --- | --- |
| [使用者指南](../GUIDE.md) | [設定](../CONFIGURATION.md) |
| [安裝](../INSTALL.md) | [架構](../ARCHITECTURE.md) |
| [提供者與本機模型](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [模式與權限](../MODES.md) | [外掛撰寫](../PLUGIN_AUTHORING.md) |
| [快速鍵](../KEYBINDINGS.md) | [收據](../RECEIPTS.md) |
| [GitHub 提取要求審查](../GITHUB_ACTION.md) | [所有文件](../README.md) |

## 參與貢獻

Codewhale 由使用它的人公開打造，使用許多種語言。

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale 貢獻者牆：每一位在 Codewhale 中留下程式碼、共同署名提交、被延續的提取要求或獲得致謝的回報的人各有一個頭像，頂端是總人數。"></a>

[貢獻者牆](../CONTRIBUTORS.md)統計了每一位在 Codewhale 中留下程式碼、共同署名提交、被延續的提取要求或獲得致謝的回報的人，包括 GitHub 自己的貢獻者清單漏掉的人。`scripts/contributor-wall.py` 會根據 git 歷史和 GitHub 重新產生這張圖和名單。

歡迎任何回饋：一個 bug、一個想法，或某處看起來、用起來不對勁的地方。這些都可以[開一個 issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose)；新功能也最好從 issue 開始。歡迎為修正、翻譯、提供者和計畫中的工作提交提取要求。問題可以發到 [Discussions](https://github.com/codewhale-hq/Codewhale/discussions)。[貢獻指南](../../CONTRIBUTING.md)說明了每種協助該去哪裡、你的工作如何以你的名字合入，以及[各部分程式碼在哪裡](../../CONTRIBUTING.md#project-structure)。

可以在 [Discord](https://discord.gg/37gfS3ksug) 上聊天，或在微信上加維護者 Hunter（`hunterbown`），申請加入 Whale Brothers 群。歡迎所有人。

## 歷史與授權

Codewhale 最初名為 `deepseek-tui`，至今仍能讀取該專案的設定和工作階段。它由獨立維護，與 DeepSeek 或任何其他模型提供者均無關聯。

[MIT](../../LICENSE)。改編自其他開源專案的程式碼列在第三方聲明中：[內含的原始碼](../../THIRD_PARTY_NOTICES.md)和[借鑑的設計](../THIRD_PARTY_NOTICES.md)。
