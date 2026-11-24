<!-- source: README.md sha256:5da2c4c9d5f9 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Agent lập trình Rust mã nguồn mở cho terminal. Hỗ trợ mọi mô hình, đội hình đa agent, sandbox hệ điều hành và biên nhận có thể kiểm chứng.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Trang web](https://codewhale.net) · [Tài liệu](../README.md) · [Nhật ký thay đổi](../../CHANGELOG.md) · [Đóng góp](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Bản dịch này là bản nháp do máy dịch và chưa được người bản ngữ xem lại.</sub>

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Codewhale v0.10.4 trong terminal sau một tác vụ thật trên Ollama cục bộ (qwen3:4b): sửa ls.py, chờ phê duyệt trước khi chạy python3 -m unittest -v test_ls.py, chạy 2 bài kiểm thử đạt và báo cáo kết quả.">

<sub>Phiên làm việc thật trên bản phát hành Codewhale v0.10.4 (48669ad108fa) chạy Ollama cục bộ (qwen3:4b) ở chế độ Accept Edits.</sub>

</div>

## Cài đặt

Tập lệnh cài đặt ở trên đặt `codewhale` đã xác minh checksum cùng bí danh ngắn `codew` vào `~/.local/bin` trên macOS và Linux. Nếu shell báo `command not found`, hãy thêm `~/.local/bin` vào `PATH` ([chi tiết](../INSTALL.md#put-it-on-your-path)). Nâng cấp tại chỗ bằng `codewhale update`.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Tệp nhị phân dựng sẵn hỗ trợ Linux, macOS và Windows (x64 và arm64). Scoop, Nix, Homebrew trên Linux, Android/Termux (bản xem trước) và tải thủ công có trong [docs/INSTALL.md](../INSTALL.md); ảnh container có trong [docs/DOCKER.md](../DOCKER.md). Chỉ dùng một phương thức cài đặt trên mỗi máy để hai tệp nhị phân không che khuất nhau trên `PATH`.

## Bắt đầu nhanh

1. Commit hoặc stash công việc hiện tại, sau đó chạy `codewhale` từ thư mục gốc của kho git.
2. Chạy `/provider` (hoặc nhấn `F3`) để chọn nhà cung cấp, dán khóa API và chọn mô hình. Mặc định là DeepSeek; nếu chưa cấu hình khóa và máy chủ Ollama cục bộ đang chạy, Codewhale sẽ đề xuất mô hình cục bộ ở tin nhắn đầu tiên.
3. Nhập tác vụ:

```text
Fix the failing tests and explain what changed.
```

Các lệnh chỉ đọc chạy ngay lập tức. Lệnh shell có thay đổi trạng thái sẽ hỏi phê duyệt: `y` chạy một lần, `a` cho phép các lệnh tương tự trong phần còn lại của phiên, `n` từ chối. Chạy `/undo` để hoàn tác lượt gần nhất và các sửa đổi tệp, hoặc `/help` để xem lệnh và phím tắt.

## Khả năng chính

- **50+ nhà cung cấp tích hợp và mô hình cục bộ:** Anthropic, OpenAI, DeepSeek (mặc định `deepseek-flash` / V4.1 Flash; [chi tiết](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, mọi endpoint tương thích OpenAI, cùng các runtime cục bộ qua Ollama, vLLM hoặc SGLang. Đổi mô hình giữa phiên bằng `/model`, định tuyến theo từng lượt bằng `/model auto` (`/router`), kiểm tra độ trễ và chi phí bằng `/turn inspect`. Xem [Nhà cung cấp](../PROVIDERS.md).
- **Biên nhận và hoàn tác:** `/receipts` (hoặc `codewhale receipts --last` sau khi thoát) ghi lại các tệp đã sửa, lệnh đã chạy, lời gọi web/MCP, agent con, phê duyệt và lỗi. `/undo` hoàn tác lượt cuối trong một bước; `/restore` đưa workspace về ảnh chụp trước đó (cần `/trust on` hoặc Full Access). Xem [Biên nhận](../RECEIPTS.md).
- **Agent con, Fleet và quy trình:** Giữ mục tiêu xuyên nhiều lượt với `/goal`, giao việc cho [agent con](../SUBAGENTS.md), điều phối [Fleet](../FLEET.md) theo vai trò và mô hình, hoặc chạy [quy trình](../WORKFLOW_AUTHORING.md) lưu trong kho mã.
- **MCP, kỹ năng, hook và plugin:** Kết nối [máy chủ MCP](../MCP.md), [kỹ năng](../SKILLS.md), [plugin](../PLUGINS.md) và [hook](../HOOKS.md) vòng đời. Chế độ mã thử nghiệm (`execute_tools`) gom nhiều lời gọi MCP/plugin dưới cùng quy tắc phê duyệt. Tiện ích tùy chọn [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) trên macOS luôn tắt cho đến khi bạn xem xét quyền và bật lên.
- **Tương thích trực tiếp:** Tự động đọc `AGENTS.md`. Bật đọc `CLAUDE.md`, `.cursorrules` hoặc `.github/copilot-instructions.md` qua `project_instruction_imports` trong [config.example.toml](../../config.example.toml). Nhập cấu hình và máy chủ MCP của Claude Code bằng `/import-claude`, hoặc cài plugin Claude Code (kỹ năng, lệnh, hồ sơ agent, MCP) và plugin DeepSeek Harness (DSH) bằng `/plugin install` ([chi tiết](../CLAUDE_PLUGIN_COMPAT.md)).

## Các cách chạy

| Lệnh | Mục đích |
| --- | --- |
| `codewhale` | Ứng dụng terminal tương tác |
| `codewhale exec "…"` | Trình chạy một tác vụ không giao diện cho script và CI ([hướng dẫn](../INSTALL.md#headless-scripts-ci); `--auto` dùng Full Access trừ khi bật sandbox) |
| `codewhale web` | Giao diện web cục bộ trên `127.0.0.1` ([hướng dẫn](../WEB.md)) |
| `codewhale review --pr N` | Đánh giá tham khảo PR GitHub; [thiết lập bot GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) chạy bằng khóa của riêng bạn |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md) HTTP cục bộ cho luồng, sự kiện, phê duyệt và diff từng lượt |
| `codewhale serve --acp` | Agent Client Protocol qua stdio cho các trình soạn thảo như Zed |

Tất cả các điểm vào dùng chung một engine Rust ([kiến trúc](../ARCHITECTURE.md)). [Tiện ích mở rộng VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) do cộng đồng duy trì ([mã nguồn](https://github.com/HengQuWorld/CodeWhale-VSCode)) kết nối tới cùng engine đó. Xem [ghi chú phát hành mới nhất](https://github.com/codewhale-hq/Codewhale/releases/latest) và [lộ trình](../ROADMAP.md) để biết trạng thái phát hành và ứng dụng desktop/cloud.

## Quyền, sandbox và quyền riêng tư

`Shift+Tab` chuyển đổi các chế độ quyền: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` chỉ dùng để hoàn thành lệnh). **Accept Edits** (mặc định) áp dụng sửa đổi tệp trong kho git và hỏi trước khi chạy lệnh. **Ask** hỏi trước mọi sửa đổi và lệnh. **Plan** chỉ đọc. **Auto-Review** dùng quy tắc và mô hình đánh giá để duyệt lệnh thường lệ và chặn thao tác rủi ro cao. **Full Access** bỏ qua lời nhắc và tắt OS sandbox (quy tắc kho mã và chính sách quản trị vẫn áp dụng). Xem [Chế độ](../MODES.md).

Trong Ask, Accept Edits và Auto-Review, lệnh chạy trong sandbox hệ điều hành khi có sẵn: Seatbelt trên macOS và bubblewrap trên Linux (Windows chưa có OS sandbox). Lệnh và phiên terminal trong sandbox không thể đọc đường dẫn thông tin xác thực như `~/.ssh`, `~/.aws` hoặc tệp `.env`. Chạy `/preview-request` để xem trước yêu cầu đã ẩn thông tin nhạy cảm trước khi gửi.

Khóa API thêm qua `/provider` chỉ lưu trên máy của bạn và mô hình cục bộ không cần khóa. `/cost` và dòng trạng thái theo dõi chi phí phiên khi biết giá (giá chưa rõ sẽ hiển thị là chưa rõ, không bao giờ hiển thị là miễn phí).

Dữ liệu đo từ xa tổng hợp được bật mặc định (phiên bản, hệ điều hành, độ dài/kết quả phiên, ID nhà cung cấp tích hợp, số đếm tính năng/lỗi; ID cài đặt ngẫu nhiên xoay vòng mỗi 90 ngày; không bao giờ gửi mã nguồn, prompt, hội thoại, tên mô hình, đường dẫn hoặc khóa). Tắt bằng lệnh:

```bash
codewhale config set telemetry false
```

Đặt `CODEWHALE_NO_UPDATE_CHECK=1` để bỏ qua bước kiểm tra bản phát hành GitHub khi khởi động. Chi tiết: [thứ tự ủy quyền](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md) và [đo từ xa](../TELEMETRY.md).

## Tài liệu

Các hướng dẫn theo sát nhánh `main` và có thể mô tả thay đổi chưa có trong bản phát hành mới nhất.

| Bắt đầu tại đây | Tài liệu tham khảo |
| --- | --- |
| [Hướng dẫn người dùng](../GUIDE.md) | [Cấu hình](../CONFIGURATION.md) |
| [Cài đặt](../INSTALL.md) | [Kiến trúc](../ARCHITECTURE.md) |
| [Nhà cung cấp và mô hình cục bộ](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Chế độ và quyền](../MODES.md) | [Viết plugin](../PLUGIN_AUTHORING.md) |
| [Phím tắt](../KEYBINDINGS.md) | [Biên nhận](../RECEIPTS.md) |
| [Đánh giá PR GitHub](../GITHUB_ACTION.md) | [Tất cả tài liệu](../README.md) |

## Đóng góp và giấy phép

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Bức tường người đóng góp Codewhale"></a>

[Bức tường người đóng góp](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) ghi nhận mã nguồn, commit đồng tác giả, PR kế thừa và báo cáo được ghi công. Báo lỗi và đề xuất tính năng tại [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), đặt câu hỏi tại [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), hướng dẫn PR và bố cục kho nằm trong [CONTRIBUTING.md](../../CONTRIBUTING.md) ([cấu trúc dự án](../../CONTRIBUTING.md#project-structure)). Tham gia [Discord](https://discord.gg/37gfS3ksug) hoặc kết bạn `hunterbown` trên WeChat để vào nhóm Whale Brothers.

Codewhale khởi nguồn từ `deepseek-tui` và giữ tương thích ngược với cấu hình và phiên của dự án đó. Dự án được duy trì độc lập và không liên kết với DeepSeek hay bất kỳ nhà cung cấp mô hình nào khác.

Giấy phép [MIT](../../LICENSE). Thông báo bên thứ ba: [mã nguồn đi kèm](../../THIRD_PARTY_NOTICES.md) và [thiết kế tham khảo](../THIRD_PARTY_NOTICES.md).
