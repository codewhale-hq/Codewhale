<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**支援任何模型的開源程式碼代理。**

Codewhale 會讀取你的專案、編輯檔案、執行指令並檢查自己的成果——
在你的終端機中，使用你選擇的託管模型或本機模型。

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[官方網站](https://codewhale.net) · [文件](docs/README.md) · [變更記錄](CHANGELOG.md) · [參與貢獻](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="一次 Codewhale 終端機工作階段" width="760">

<sub>已設定的本機示範工作階段中的真實 Codewhale 終端機截圖。</sub>

</div>

## 安裝

macOS 與 Linux：

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

安裝程式會將經過校驗碼驗證的二進位檔下載到 `~/.local/bin`。如果之後執行
`codewhale` 顯示 "command not found"，請執行安裝程式印出的那一行 PATH 指令，或參閱
[將它加入 PATH](docs/INSTALL.md#put-it-on-your-path)。
隨時可以使用 `codewhale update` 升級。

<details>
<summary><b>Windows、npm、Cargo 與其他安裝方式</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker、Nix、Linux 上的 Homebrew、Android/Termux、附校驗碼驗證的手動下載，
以及選用的 CNB 鏡像，都在[安裝指南](docs/INSTALL.md)中說明。請只選擇一種方式：
同一台機器上安裝多份，最後會在 `PATH` 上互相衝突。

</details>

## 快速開始

**在同一個地方管理你的模型 API 金鑰。** [註冊 Codewhale 帳號](https://app.codewhale.net/register)，
或執行 `codewhale login` 登入。將 API 金鑰儲存至帳號後，即可集中更新，並在已登入的裝置上
透過 Codewhale 模型路由使用。[設定帳號金鑰](docs/CONFIGURATION.md#account-provider-keys)。
登入不會上傳既有的本機金鑰；不註冊帳號也可以繼續在本機使用。

1. **開啟你的專案。** 在要處理的資料夾中執行 `codewhale`。
2. **連接模型。** 執行 `/provider`（或按 `F3`）新增託管服務的金鑰，或選擇本機執行環境。
   如果 Ollama 已在執行並載入聊天模型，Codewhale 會自動切換到它。使用 `/model` 更換模型。
3. **給它一個具體的任務。**

```text
Fix the failing tests and explain what changed.
```

同一項任務也可以在腳本或 CI 工作中以無介面方式執行：

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

執行 `/help` 查看指令與鍵盤快速鍵。

## 執行方式

所有用戶端都驅動同一個本機 Codewhale Runtime，因此工作階段、工具與權限在各處的行為一致。

| 指令 | 作用 |
| --- | --- |
| `codewhale` | 互動式終端機介面 |
| `codewhale exec "…"` | 在腳本或 CI 中執行一次無介面回合；`--auto` 允許使用檔案和 shell 工具，`--output-format stream-json` 以串流輸出 JSON |
| `codewhale web` | 內建的[本機瀏覽器用戶端](docs/WEB.md)，監聽 `127.0.0.1` |
| `codewhale review --pr N` | 僅供參考的[拉取請求審查](docs/GITHUB_ACTION.md)；是否張貼留言需主動開啟 |
| Runtime API | 用於執行緒、事件與核准的[本機 HTTP API](docs/RUNTIME_API.md) |

原生桌面應用程式（GPUI）正作為需登入的產品用戶端開發中；開放情況請見
[產品頁面](https://codewhale.net/en/product)。由社群維護的
[VS Code 擴充功能](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
可在側邊欄連接同一個 Runtime（[原始碼](https://github.com/HengQuWorld/CodeWhale-VSCode)）。

## 功能

- **任何模型，不被綁定。** 內建超過 40 條供應商路由——Anthropic、DeepSeek、Google、
  Mistral、Moonshot、OpenAI、OpenRouter、xAI 等——另支援任何相容 OpenAI 的端點，
  以及透過 Ollama、vLLM 或 SGLang 執行的本機模型。[供應商](docs/PROVIDERS.md)
- **由你掌控。** Plan 模式只探索、不做任何變更；Work 與 Operate 會進行修改。
  核准姿態決定工具呼叫何時需要你的同意，`/undo` 與 `/restore` 可還原工作區變更，
  `/receipts` 會列出一次工作階段中的每個檔案、指令與核准。[模式](docs/MODES.md) ·
  [回條](docs/RECEIPTS.md)
- **為長時間任務而設計。** 設定持久的 `/goal`，把有範圍的工作委派給
  [子代理](docs/SUBAGENTS.md)，執行附有預先花費檢查的受監督[代理團隊](docs/FLEET.md)，
  或將它們撰寫為可納入版本庫的[工作流程](docs/WORKFLOW_AUTHORING.md)。
- **擴充你已在使用的工具。** 連接 [MCP 伺服器](docs/MCP.md)，安裝
  [技能](docs/SKILLS.md)與[外掛](docs/PLUGINS.md)，在工作階段與工具事件上執行
  [Hook](docs/HOOKS.md)，並載入現有的 [Claude Code 外掛](docs/CLAUDE_PLUGIN_COMPAT.md)。
- **Computer Use。** 內建外掛提供觀察與操作其他應用程式的工具。使用前請先檢視其存取範圍並啟用。
  [指南](crates/tui/plugins/computer-use/README.md)

## 模式與權限

| | 切換方式 | 選項 |
| --- | --- | --- |
| **模式（Mode）**——代理正在做什麼 | `Tab` 或 `/mode` | Plan（探索，不做變更）· Work（編輯並執行）· Operate（透過有計畫、可驗證的步驟推進一個目標） |
| **姿態（Posture）**——何時先徵求同意 | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access 仍然遵守硬性政策邊界。
[模式與權限指南](docs/MODES.md)對每個選項都有說明。

## 安全

Codewhale 在你的機器上執行，只擁有你授予它的存取權限。核准姿態與儲存庫規則會限制代理可以做的事，
在支援的平台上，指令會在作業系統沙箱中執行（macOS 上為 Seatbelt；Linux 上的 bubblewrap 在已安裝且可用時）。
`/preview-request` 會在送出任何內容之前顯示經過遮蔽的完整請求。
未知的模型價格會維持「未知」，而不會被回報為免費。

請參閱[授權順序](docs/AUTHORIZATION_ORDER.md)、
[沙箱](docs/SANDBOX.md)與[遙測](docs/TELEMETRY.md)——使用量計數預設開啟，
執行 `codewhale config set telemetry false` 即可關閉。

## 文件

| 從這裡開始 | 深入了解 |
| --- | --- |
| [安裝](docs/INSTALL.md) | [設定](docs/CONFIGURATION.md) |
| [供應商與本機模型](docs/PROVIDERS.md) | [架構](docs/ARCHITECTURE.md) |
| [模式與權限](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [鍵盤快速鍵](docs/KEYBINDINGS.md) | [外掛開發](docs/PLUGIN_AUTHORING.md) |
| [GitHub PR 審查](docs/GITHUB_ACTION.md) | [所有文件](docs/README.md) |

## 社群

歡迎提交錯誤回報、功能想法與拉取請求——無論你已使用 Codewhale 數月，還是第一次嘗試。
如果缺少某個供應商，或某個工作流程不順手，
請[提出 issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose)或
[送出拉取請求](CONTRIBUTING.md)。歡迎首次貢獻，
被合併的成果會保留貢獻者的署名。
[儲存庫結構](CONTRIBUTING.md#project-structure)是個不錯的起點。

歡迎加入 [Discord](https://discord.gg/37gfS3ksug)，或在微信上加 Hunter
（`hunterbown`）為好友，申請加入 Whale Brothers 群組。

## 歷史與授權

Codewhale 最初名為 `deepseek-tui`，至今仍會讀取該專案的設定與工作階段。
它現在不綁定特定供應商，由獨立團隊維護，與任何模型供應商均無隸屬關係。
感謝[每一位貢獻者](docs/CONTRIBUTORS.md)，以及協助它成長的開源社群。

[MIT](LICENSE)。改編自其他開源專案的部分記錄在
[第三方聲明](docs/THIRD_PARTY_NOTICES.md)中。
