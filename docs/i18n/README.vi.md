<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>Mô hình có thể là của bất kỳ ai. Agent nên là của bạn.</h1>

Codewhale là một agent lập trình mã nguồn mở cho terminal của bạn, được viết bằng Rust. Nó làm việc với bất kỳ mô hình nào, có thể chia một nhiệm vụ cho nhiều agent, và bạn có thể đọc từng dòng mã của nó.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Lệnh này cài bản phát hành mới nhất trên macOS hoặc Linux và không cần tài khoản. [Windows, npm và Cargo](#cài-đặt) ở bên dưới.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Trang web](https://codewhale.net) · [Tài liệu](../README.md) · [Nhật ký thay đổi](../../CHANGELOG.md) · [Đóng góp](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Bản dịch này là bản nháp do máy dịch và chưa được người bản ngữ xem lại.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale trong terminal sau một nhiệm vụ demo. Nó báo đã thêm cờ --json vào export.py, cả 3 bài kiểm thử cục bộ đều đạt và bài review được ủy quyền không tìm thấy vấn đề. Danh sách nhiệm vụ bên dưới có ba bước đã hoàn thành, và dòng trạng thái hiển thị Full Access cùng nhà cung cấp cục bộ website-demo.">

<sub>Một bản dựng phát triển của 0.10.3, phiên bản chưa phát hành, với mô hình kiểm thử theo kịch bản, chạy ở Full Access trong một workspace dùng một lần. Phần chỉnh sửa, ba bài kiểm thử và một lần review của sub-agent đều chạy thật.</sub>

</div>

## Cài đặt

Lệnh ở trên đặt `codewhale` đã kiểm tra checksum và bí danh ngắn `codew` vào `~/.local/bin`. Nếu shell sau đó báo `command not found`, hãy làm theo ghi chú PATH mà trình cài đặt in ra ([chi tiết](../INSTALL.md#put-it-on-your-path)). `codewhale update` sẽ nâng cấp nó.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Bản nhị phân dựng sẵn có cho Linux, macOS và Windows trên x64 và arm64. Scoop, Nix, Homebrew trên Linux, Android và Termux (bản xem trước) cùng cách tải thủ công có trong [hướng dẫn cài đặt](../INSTALL.md), còn container có trong [Docker](../DOCKER.md). Mỗi máy chỉ nên dùng một cách cài; nếu không, hai bản sẽ cùng nằm trên `PATH` và bản nào đứng trước sẽ được dùng.

## Bắt đầu nhanh

1. Commit hoặc stash công việc của bạn, rồi chạy `codewhale` từ thư mục gốc của một kho git. Mặc định, nó áp dụng các chỉnh sửa tệp trong kho mà không hỏi, và hỏi trước mỗi lệnh shell không chỉ đọc.
2. Chạy `/provider` (hoặc nhấn `F3`), chọn một nhà cung cấp, dán khóa API và chọn mô hình. Nhà cung cấp mặc định là DeepSeek. Ở lần khởi chạy đầu tiên khi chưa có khóa, Codewhale hỏi nhà cung cấp khi bạn gửi tin nhắn đầu tiên, và đề xuất một mô hình cục bộ khi máy chủ Ollama cục bộ phản hồi.
3. Nhập một nhiệm vụ:

```text
Fix the failing tests and explain what changed.
```

Lệnh chỉ đọc chạy ngay. Với mọi lệnh shell khác, Codewhale hỏi trước: `y` chạy một lần, `a` cho phép các lệnh tương tự trong phần còn lại của cuộc trò chuyện, và `n` từ chối. `/undo` hoàn tác yêu cầu cuối cùng cùng các thay đổi tệp của nó, và `/help` liệt kê lệnh và phím.

Với script và CI, `codewhale exec` chạy một nhiệm vụ mà không cần giao diện. Hãy đọc [chạy không giao diện](../INSTALL.md#headless-scripts-ci) trước khi cho nó công cụ bằng `--auto`, vì `--auto` dùng Full Access trừ khi bạn thêm sandbox.

## Nó làm gì

Giao cho Codewhale một nhiệm vụ, nó sẽ đọc kho mã, chỉnh sửa tệp và chạy lệnh cho đến khi xong hoặc cần bạn nhập thêm. [Ghi chú phát hành mới nhất](https://github.com/codewhale-hq/Codewhale/releases/latest) liệt kê các thay đổi mới nhất, và [nhật ký thay đổi](../../CHANGELOG.md) có mọi phiên bản.

### Bất kỳ mô hình nào

Hơn 40 nhà cung cấp được tích hợp sẵn, gồm Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter và xAI. Mọi endpoint tương thích OpenAI đều dùng được, và cả mô hình cục bộ qua Ollama, vLLM hoặc SGLang. Mô hình mặc định là `deepseek-flash` của DeepSeek, hiện do DeepSeek V4.1 Flash phục vụ ([chi tiết](../PROVIDERS.md#deepseek-v41-flash)).

`/model` đổi mô hình giữa các lượt, và `/model auto` chọn một mô hình cho mỗi lượt (`/router` thiết lập cách chọn). `/turn inspect` cho biết một lượt đã dùng mô hình nào, tốn bao nhiêu và mất bao lâu. Xem [Nhà cung cấp](../PROVIDERS.md).

### Biên nhận và hoàn tác

`/receipts`, hoặc `codewhale receipts --last` sau khi thoát, liệt kê các tệp một phiên đã thay đổi, các lệnh đã chạy, các lời gọi web và MCP, các agent đã khởi động, mỗi lần phê duyệt và người phê duyệt, và những gì thất bại. `/undo` hoàn tác yêu cầu cuối cùng cùng các tệp nó đã thay đổi trong một bước. `/restore` đưa toàn bộ workspace về một snapshot trước đó, và vì việc này ghi đè các chỉnh sửa sau đó nên cần `/trust on` hoặc Full Access. Xem [Biên nhận](../RECEIPTS.md).

### Nhiều agent trên một nhiệm vụ

`/goal` đặt một mục tiêu kéo dài qua nhiều lượt. Codewhale có thể chia việc cho các [sub-agent](../SUBAGENTS.md), chạy một [Fleet](../FLEET.md) gồm các agent với vai trò và mô hình riêng, hoặc làm theo một [quy trình làm việc](../WORKFLOW_AUTHORING.md) được commit trong kho.

### Tiện ích mở rộng

Kết nối [máy chủ MCP](../MCP.md), thêm [kỹ năng](../SKILLS.md) và [plugin](../PLUGINS.md), và chạy [hook](../HOOKS.md) trên các sự kiện phiên và công cụ. Chế độ mã (thử nghiệm, bật mặc định) cho phép mô hình viết một chương trình `execute_tools` gọi nhiều công cụ MCP và plugin; mỗi lời gọi vẫn tuân theo quy tắc phê duyệt của bạn và giữ biên nhận. Plugin Computer Use đi kèm v0.10.2 (thử nghiệm, chỉ macOS) có thể quan sát và điều khiển các ứng dụng khác, và nó luôn tắt cho đến khi bạn xem lại quyền truy cập và bật nó ([hướng dẫn](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). Sắp có trong 0.10.3: plugin này được [phát hành riêng](https://github.com/codewhale-hq/codewhale-cu-plugin).

### Các cách chạy

| Lệnh | Tác dụng |
| --- | --- |
| `codewhale` | Ứng dụng terminal tương tác |
| `codewhale exec "…"` | Một nhiệm vụ không cần giao diện, cho script và CI ([chạy không giao diện](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | Một ứng dụng web cục bộ trên `127.0.0.1` ([hướng dẫn](../WEB.md)) |
| `codewhale review --pr N` | Một bài review mang tính tư vấn cho pull request trên GitHub; [cài đặt bot GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) chạy nó bằng khóa mô hình của chính bạn |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md), một HTTP API cục bộ cho luồng, sự kiện, phê duyệt và thay đổi tệp của từng lượt |
| `codewhale serve --acp` | Agent Client Protocol qua stdio, cho các trình soạn thảo như Zed |

Ứng dụng terminal, `exec`, `web` và Runtime API dùng chung một động cơ ([kiến trúc](../ARCHITECTURE.md)). Một [tiện ích VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) do cộng đồng duy trì kết nối với nó từ thanh bên ([mã nguồn](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Cách nó giữ an toàn

Hai công tắc quyết định Codewhale được tự làm bao nhiêu. Khi ô nhập trống, `Tab` đổi chế độ: **Plan** chỉ đọc và tìm kiếm, không thay đổi gì, **Work** chỉnh sửa tệp và chạy lệnh, còn **Operate** có cùng quyền như Work và tiếp tục cho đến khi mục tiêu được xác minh hoặc cần đến bạn. `Shift+Tab` đổi quyền. **Ask**, mặc định, áp dụng chỉnh sửa tệp trong kho mà không hỏi và hỏi trước các lệnh shell không chỉ đọc. **Auto-Review** để các quy tắc và một mô hình duyệt phê duyệt các lệnh gọi thông thường và từ chối lệnh rủi ro cao; chuyển sang Ask để tự phê duyệt chúng. **Full Access** không bao giờ hỏi và tắt sandbox của Codewhale, dù quy tắc của kho và chính sách quản lý vẫn áp dụng. [Hướng dẫn v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md) có chi tiết.

Sắp có trong 0.10.3: `Shift+Tab` trở thành một núm xoay qua Ask, Accept Edits, Plan, Auto-Review và Full Access, `Tab` không còn đổi chế độ, và Operate bị loại bỏ. Ask sẽ hỏi trước cả mỗi chỉnh sửa tệp, còn mặc định mới, Accept Edits, sẽ áp dụng chỉnh sửa trong kho git và vẫn hỏi trước các lệnh. [Hướng dẫn hiện tại](../MODES.md) mô tả điều này.

Ở Ask và Auto-Review, lệnh chạy trong sandbox của hệ điều hành nếu hệ thống có: Seatbelt trên macOS, và bubblewrap trên Linux khi đã cài. Windows chưa có sandbox hệ điều hành, và Full Access tắt sandbox. Bên trong sandbox, lệnh shell và phiên terminal không thể đọc các kho thông tin xác thực như `~/.ssh`, `~/.aws` và tệp `.env`. `/preview-request` hiển thị bản xem trước đã che thông tin nhạy cảm của yêu cầu tiếp theo mà không gửi nó.

Bạn không cần tài khoản, và khóa bạn thêm bằng `/provider` ở lại trên máy của bạn. Với khóa API của riêng bạn, bạn trả tiền trực tiếp cho nhà cung cấp, còn mô hình cục bộ không cần khóa. Chân trang và `/cost` cho biết một phiên đã tốn bao nhiêu khi biết giá, và giá chưa biết được hiển thị là chưa biết, không bao giờ là miễn phí.

Telemetry được bật mặc định. Nó gửi các số liệu tổng hợp (phiên bản và nền tảng, độ dài và kết quả phiên, các nhà cung cấp tích hợp bạn đã dùng, số lần dùng tính năng và số lỗi) kèm một ID cài đặt ngẫu nhiên đổi mỗi 90 ngày, và không bao giờ gửi cuộc trò chuyện, mã, prompt, tên mô hình, tên tệp hay tên kho, hoặc thông tin xác thực. Lệnh này tắt nó:

```bash
codewhale config set telemetry false
```

Khi khởi động, Codewhale cũng kiểm tra GitHub để tìm bản phát hành mới; đặt `CODEWHALE_NO_UPDATE_CHECK=1` để bỏ qua. Xem thêm [thứ tự cấp quyền](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md) và [telemetry](../TELEMETRY.md).

## Hướng đi sắp tới

Codewhale đang trở thành một môi trường duy nhất trải khắp terminal, ứng dụng máy tính, trình duyệt và máy tính đám mây của bạn, tất cả chạy bằng một động cơ. Mỗi phần bên dưới cho biết hôm nay nó đang ở đâu.

| Phần | Trạng thái | Hôm nay |
| --- | --- | --- |
| Ứng dụng terminal | Đã phát hành | Cài trên macOS, Linux hoặc Windows và chạy `codewhale` trong bất kỳ kho mã nào. |
| Ứng dụng khách trình duyệt cục bộ | Đã phát hành | Chạy `codewhale web` để đọc cuộc trò chuyện, gửi nhiệm vụ và trả lời yêu cầu phê duyệt ngay trong trình duyệt; nó đi kèm terminal và không cần tài khoản. |
| Nhiều agent trên một nhiệm vụ | Đã phát hành | Sub-agent, Fleet và quy trình làm việc cho phép nhiều agent chia sẻ một nhiệm vụ, mỗi agent một vai trò. |
| Ứng dụng máy tính | Đang phát triển, chưa có bản tải | Chúng tôi đang xây dựng một ứng dụng máy tính trên cùng động cơ, và nó chưa có bản tải công khai. |
| Máy tính đám mây | Chỉ theo lời mời | Máy tính đám mây chỉ dành cho người được mời, và chúng tôi đang xây dựng chúng để một phiên có thể tiếp tục làm việc trên một máy trong đám mây. |
| Một phiên xuyên suốt tất cả | Đang xây dựng | Chúng tôi đang xây dựng một phiên đi cùng bạn từ terminal đến máy tính, trình duyệt và đám mây, và vẫn là của bạn suốt chặng đường. |

[Lộ trình](../ROADMAP.md) cho biết mỗi bản phát hành bao gồm những gì và bạn có thể giúp ở đâu.

## Chuyển từ agent khác sang

Codewhale tự đọc `AGENTS.md`. Các tệp hướng dẫn viết cho công cụ khác, như `CLAUDE.md`, `.cursorrules` hoặc `.github/copilot-instructions.md`, chỉ được nạp khi bạn chủ động bật bằng `project_instruction_imports = ["claude"]` (hoặc `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) trong cấu hình; xem [config.example.toml](../../config.example.toml). Codewhale cảnh báo khi tìm thấy một tệp như vậy mà nó không nạp.

`/import-claude` chuẩn bị việc nhập cài đặt Claude Code và máy chủ MCP của bạn để bạn xem lại. `/plugin install` chấp nhận plugin Claude Code chứa kỹ năng, lệnh, hồ sơ agent và máy chủ MCP, và từ chối plugin cần hook hoặc máy chủ LSP ([chi tiết](../CLAUDE_PLUGIN_COMPAT.md)).

## Tài liệu

Các hướng dẫn này đi theo nhánh chính và có thể mô tả những thay đổi chưa có trong bản phát hành mới nhất.

| Bắt đầu tại đây | Tham khảo |
| --- | --- |
| [Hướng dẫn sử dụng](../GUIDE.md) | [Cấu hình](../CONFIGURATION.md) |
| [Cài đặt](../INSTALL.md) | [Kiến trúc](../ARCHITECTURE.md) |
| [Nhà cung cấp và mô hình cục bộ](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Chế độ và quyền](../MODES.md) | [Viết plugin](../PLUGIN_AUTHORING.md) |
| [Phím tắt](../KEYBINDINGS.md) | [Biên nhận](../RECEIPTS.md) |
| [Review pull request trên GitHub](../GITHUB_ACTION.md) | [Toàn bộ tài liệu](../README.md) |

## Đóng góp

Codewhale được xây dựng công khai bởi những người dùng nó, bằng nhiều ngôn ngữ.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Bức tường người đóng góp của Codewhale: mỗi người có mã, commit đồng tác giả, pull request được tiếp nối hoặc báo cáo được ghi công trong Codewhale có một ảnh đại diện, với tổng số người ở trên cùng."></a>

[Bức tường người đóng góp](../CONTRIBUTORS.md) tính mọi người có mã, commit đồng tác giả, pull request được tiếp nối hoặc báo cáo được ghi công trong Codewhale, kể cả những người mà danh sách người đóng góp của chính GitHub bỏ sót. `scripts/contributor-wall.py` tạo lại hình và danh sách từ lịch sử git và GitHub.

Mọi phản hồi đều được hoan nghênh: một lỗi, một ý tưởng, hoặc điều gì đó trông hay dùng thấy không ổn. [Mở một issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) cho bất kỳ điều nào trong số đó; issue cũng là nơi tốt nhất để bắt đầu một tính năng mới. Pull request được hoan nghênh cho bản sửa lỗi, bản dịch, nhà cung cấp và công việc đã lên kế hoạch. Câu hỏi hãy gửi lên [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). [Hướng dẫn đóng góp](../../CONTRIBUTING.md) cho biết mỗi kiểu giúp đỡ nên đi đâu, công việc của bạn được hợp nhất mang tên bạn như thế nào, và [mọi thứ nằm ở đâu](../../CONTRIBUTING.md#project-structure).

Trò chuyện trên [Discord](https://discord.gg/37gfS3ksug), hoặc thêm người bảo trì, Hunter, trên WeChat (`hunterbown`) và xin vào nhóm Whale Brothers. Mọi người đều được chào đón.

## Lịch sử và giấy phép

Codewhale bắt đầu với tên `deepseek-tui` và vẫn đọc được cấu hình và phiên của dự án đó. Nó được duy trì độc lập và không liên kết với DeepSeek hay bất kỳ nhà cung cấp mô hình nào khác.

[MIT](../../LICENSE). Mã được điều chỉnh từ các dự án mã nguồn mở khác được liệt kê trong thông báo bên thứ ba cho [mã nguồn được đưa vào](../../THIRD_PARTY_NOTICES.md) và [thiết kế được tham khảo](../THIRD_PARTY_NOTICES.md).
