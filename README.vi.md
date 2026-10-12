<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Tác nhân lập trình mã nguồn mở, hoạt động với mọi mô hình.**

Codewhale đọc dự án của bạn, chỉnh sửa tệp, chạy lệnh và tự kiểm tra công việc
của mình, ngay trong terminal, với mô hình lưu trữ trực tuyến hoặc mô hình cục
bộ do bạn chọn.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Trang web](https://codewhale.net) · [Tài liệu](docs/README.md) · [Nhật ký thay đổi](CHANGELOG.md) · [Đóng góp](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Một phiên terminal của Codewhale" width="760">

<sub>Ảnh chụp terminal Codewhale thật từ một phiên demo cục bộ đã được cấu hình.</sub>

</div>

## Cài đặt

macOS và Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Trình cài đặt tải các tệp nhị phân đã được xác minh checksum vào `~/.local/bin`.
Nếu sau đó `codewhale` báo "command not found", hãy chạy dòng lệnh PATH mà trình
cài đặt in ra, hoặc xem
[Thêm vào PATH](docs/INSTALL.md#put-it-on-your-path).
Bạn có thể nâng cấp bất cứ lúc nào bằng `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo và các cách cài khác</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew trên Linux, Android/Termux, tải thủ công kèm xác minh
checksum và bản mirror CNB tùy chọn được trình bày trong
[hướng dẫn cài đặt](docs/INSTALL.md). Hãy chọn một cách: cài nhiều lần trên cùng
một máy sẽ xung đột với nhau về `PATH`.

</details>

## Bắt đầu nhanh

**Quản lý khóa API của các nhà cung cấp ở một nơi.** [Tạo tài khoản Codewhale](https://app.codewhale.net/register)
hoặc chạy `codewhale login` để đăng nhập. Lưu và cập nhật khóa API trong tài khoản,
rồi sử dụng chúng qua tuyến mô hình Codewhale trên các thiết bị đã đăng nhập.
[Thiết lập khóa trong tài khoản](docs/CONFIGURATION.md#account-provider-keys).
Đăng nhập không tải các khóa cục bộ hiện có lên tài khoản; bạn vẫn có thể sử dụng cục bộ mà không cần tài khoản.

1. **Mở dự án của bạn.** Chạy `codewhale` trong thư mục bạn muốn làm việc.
2. **Kết nối mô hình.** Chạy `/provider` (hoặc nhấn `F3`) để thêm khóa của mô
   hình lưu trữ trực tuyến hoặc chọn runtime cục bộ. Nếu Ollama đang chạy sẵn
   với một mô hình trò chuyện, Codewhale sẽ tự chuyển sang mô hình đó. Dùng
   `/model` để đổi mô hình.
3. **Giao một tác vụ cụ thể.**

```text
Fix the failing tests and explain what changed.
```

Cùng tác vụ đó có thể chạy ở chế độ headless từ một script hoặc job CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Chạy `/help` để xem các lệnh và phím tắt.

## Các cách chạy

Mọi client đều điều khiển cùng một Codewhale Runtime cục bộ, nên phiên làm việc,
công cụ và quyền hạn hoạt động giống nhau ở mọi nơi.

| Lệnh | Chức năng |
| --- | --- |
| `codewhale` | Giao diện terminal tương tác |
| `codewhale exec "…"` | Một lượt chạy headless từ script hoặc CI; `--auto` cho phép công cụ tệp và shell, `--output-format stream-json` phát trực tiếp JSON |
| `codewhale web` | [Client trình duyệt cục bộ](docs/WEB.md) đi kèm tại `127.0.0.1` |
| `codewhale review --pr N` | [Đánh giá pull request](docs/GITHUB_ACTION.md) mang tính tham khảo; việc đăng là tùy chọn |
| Runtime API | [API HTTP cục bộ](docs/RUNTIME_API.md) cho thread, sự kiện và phê duyệt |

Ứng dụng desktop gốc (GPUI) đang được xây dựng làm client sản phẩm cho tài khoản
đã đăng nhập; xem tình trạng phát hành tại
[trang sản phẩm](https://codewhale.net/en/product).
[Tiện ích mở rộng VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
do cộng đồng duy trì kết nối với cùng Runtime từ thanh bên
([mã nguồn](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Codewhale làm được gì

- **Mọi mô hình, không bị khóa.** Hơn 40 tuyến nhà cung cấp tích hợp sẵn,
  gồm Anthropic, DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter, xAI
  và nhiều hơn nữa, cùng mọi endpoint tương thích OpenAI và mô hình cục bộ qua
  Ollama, vLLM hoặc SGLang. [Nhà cung cấp](docs/PROVIDERS.md)
- **Bạn luôn nắm quyền kiểm soát.** Chế độ Plan chỉ khám phá mà không thay đổi
  gì; Work và Operate thực hiện thay đổi. Các posture phê duyệt quyết định khi
  nào một lệnh gọi công cụ cần bạn đồng ý, `/undo` và `/restore` khôi phục các
  thay đổi trong không gian làm việc, còn `/receipts` liệt kê mọi tệp, lệnh và
  phê duyệt trong một phiên. [Chế độ](docs/MODES.md) ·
  [Biên nhận](docs/RECEIPTS.md)
- **Dành cho công việc dài hơi.** Đặt một `/goal` bền vững, giao việc có phạm vi
  rõ ràng cho [tác nhân phụ](docs/SUBAGENTS.md), chạy
  [nhóm tác nhân](docs/FLEET.md) có giám sát với bước kiểm tra chi phí trước khi
  chạy, hoặc viết script cho chúng dưới dạng
  [workflow](docs/WORKFLOW_AUTHORING.md) lưu trong kho mã.
- **Mở rộng những gì bạn đang dùng.** Kết nối [máy chủ MCP](docs/MCP.md), cài
  [skill](docs/SKILLS.md) và [plugin](docs/PLUGINS.md), chạy
  [hook](docs/HOOKS.md) theo sự kiện phiên và công cụ, đồng thời nạp các
  [plugin Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md) hiện có.
- **Computer Use.** Một plugin đi kèm bổ sung công cụ để quan sát và điều khiển
  các ứng dụng khác. Hãy xem lại phạm vi truy cập của nó và bật trước khi dùng.
  [Hướng dẫn](crates/tui/plugins/computer-use/README.md)

## Chế độ và quyền

| | Chọn bằng | Tùy chọn |
| --- | --- | --- |
| **Chế độ** — tác nhân đang làm gì | `Tab` hoặc `/mode` | Plan (khám phá, không thay đổi) · Work (chỉnh sửa và chạy) · Operate (thực hiện một mục tiêu qua các bước đã lập kế hoạch và được kiểm chứng) |
| **Posture** — khi nào hỏi trước | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access vẫn tuân thủ các ranh giới chính sách cứng.
[Hướng dẫn về chế độ và quyền](docs/MODES.md) giải thích từng tùy chọn.

## An toàn

Codewhale chạy trên máy của bạn với quyền truy cập bạn cấp. Các posture phê duyệt
và quy tắc của kho mã giới hạn những gì tác nhân được phép làm, và lệnh chạy bên
trong sandbox của hệ điều hành ở nơi được hỗ trợ (Seatbelt trên macOS; bubblewrap
trên Linux khi đã cài đặt và hoạt động). `/preview-request` hiển thị chính xác yêu cầu đã được che
thông tin nhạy cảm trước khi bất cứ thứ gì được gửi đi. Giá của mô hình chưa biết
vẫn được giữ là chưa biết, không bị báo cáo là miễn phí.

Xem [thứ tự cấp quyền](docs/AUTHORIZATION_ORDER.md),
[sandbox](docs/SANDBOX.md) và [telemetry](docs/TELEMETRY.md): số liệu sử dụng được
bật theo mặc định và `codewhale config set telemetry false` sẽ tắt chúng.

## Tài liệu

| Bắt đầu tại đây | Tìm hiểu sâu hơn |
| --- | --- |
| [Cài đặt](docs/INSTALL.md) | [Cấu hình](docs/CONFIGURATION.md) |
| [Nhà cung cấp và mô hình cục bộ](docs/PROVIDERS.md) | [Kiến trúc](docs/ARCHITECTURE.md) |
| [Chế độ và quyền](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Phím tắt](docs/KEYBINDINGS.md) | [Viết plugin](docs/PLUGIN_AUTHORING.md) |
| [Đánh giá PR trên GitHub](docs/GITHUB_ACTION.md) | [Toàn bộ tài liệu](docs/README.md) |

## Cộng đồng

Chúng tôi hoan nghênh báo lỗi, ý tưởng tính năng và pull request, dù bạn đã dùng
Codewhale nhiều tháng hay mới thử lần đầu. Nếu thiếu một nhà cung cấp hoặc một
quy trình làm việc còn vướng víu, hãy
[mở một issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) hoặc
[gửi pull request](CONTRIBUTING.md). Đóng góp đầu tiên luôn được chào đón, và
người đóng góp được ghi nhận công sức cho phần việc được tích hợp.
[Cấu trúc kho mã](CONTRIBUTING.md#project-structure) là nơi tốt để bắt đầu.

Hãy tham gia [Discord](https://discord.gg/37gfS3ksug), hoặc thêm Hunter trên
WeChat (`hunterbown`) và xin vào nhóm Whale Brothers.

## Lịch sử và giấy phép

Codewhale bắt đầu từ `deepseek-tui` và vẫn đọc cấu hình cùng các phiên của dự án
đó. Nay dự án không phụ thuộc nhà cung cấp mô hình nào, được duy trì độc lập và
không liên kết với bất kỳ nhà cung cấp mô hình nào. Cảm ơn
[mọi người đóng góp](docs/CONTRIBUTORS.md) và các cộng đồng mã nguồn mở đã giúp
dự án lớn mạnh.

[MIT](LICENSE). Các phần được điều chỉnh từ những dự án mã nguồn mở khác được ghi
lại trong [thông báo bên thứ ba](docs/THIRD_PARTY_NOTICES.md).
