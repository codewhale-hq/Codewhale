import type { HomeDict } from "../types";

/** Vietnamese home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale: tác nhân lập trình mã nguồn mở",
  metaDescription: "Codewhale là tác nhân lập trình mã nguồn mở trong terminal. Đọc, sửa và kiểm thử dự án với mô hình từ xa hoặc cục bộ của bạn.",
  heroTitle: "Tác nhân lập trình mã nguồn mở cho mọi mô hình",
  heroIntro: "{brand} đọc dự án, chỉnh sửa tệp và chạy kiểm thử từ terminal của bạn. Kết nối mô hình được lưu trữ trực tuyến hoặc mô hình cục bộ và chọn những hành động cần bạn phê duyệt.",
  getCodewhale: "Cài đặt Codewhale",
  heroInstallAria: "Lệnh cài đặt",
  exploreProduct: "Khám phá Codewhale",
  shotPreview: "Xem trước terminal",
  screenshotAlt:
    "Codewhale v{version}: ảnh chụp terminal với cuộc trò chuyện, ô nhập tin nhắn và điều khiển phiên.",
  latestRelease: "Bản phát hành mới nhất {tag}",
  releaseUnavailable: "Không có trạng thái phát hành",
  proofContributors: "{count} người đóng góp",
  currentSource: "Mã nguồn",
  sourceCandidate: "Chưa phát hành",
  publishedRelease: "đã phát hành",
  gainHeading: "Xây dựng và tự động hóa từ terminal",
  gainLede:
    "Mô tả những gì bạn muốn xây dựng hoặc tự động hóa. Codewhale có thể chỉnh sửa tệp, chạy lệnh và kiểm tra kết quả, với quyền truy cập do bạn kiểm soát.",
  gain: [
    [
      "Tạo ứng dụng và công cụ",
      "Tạo ứng dụng, thêm tính năng hoặc viết tập lệnh. Codewhale có thể làm việc với các tệp dự án, chạy mã và kiểm thử những gì nó tạo ra."
    ],
    [
      "Tự động hóa việc lặp lại",
      "Chạy quy trình từ terminal, tập lệnh hoặc CI. Với những tác vụ lớn hơn, giao từng phần công việc cho một Fleet tác nhân sử dụng các mô hình khác nhau."
    ],
    [
      "Kết nối công cụ bạn dùng",
      "Thêm công cụ qua plugin và máy chủ MCP, hoặc sử dụng API từ các tập lệnh của riêng bạn. Mỗi dịch vụ cần được thiết lập và xác thực riêng."
    ]
  ],
  chapterModels: "Mô hình của bạn",
  modelsHeading: "Sử dụng mô hình bạn chọn",
  modelsBody:
    "Kết nối tài khoản nhà cung cấp, endpoint tương thích với OpenAI, hoặc mô hình cục bộ và mô hình trên hạ tầng tự quản. Chọn mô hình cho phiên làm việc và cho từng tác nhân trong Fleet.",
  modelsFacts: [
    [
      "Tài khoản API của bạn",
      "Kết nối các nhà cung cấp như DeepSeek, OpenAI, Anthropic hoặc Google bằng khóa riêng. Mô hình mặc định deepseek-flash hiện chạy DeepSeek-V4.1-Flash."
    ],
    [
      "Gateway của bạn",
      "Dùng endpoint tương thích OpenAI và chọn mô hình nó cung cấp."
    ],
    [
      "Suy luận của bạn",
      "Chạy mô hình cục bộ hoặc tự lưu trữ bằng Ollama, vLLM hoặc SGLang."
    ],
    [
      "Gói ChatGPT của bạn",
      "Đăng nhập bằng codewhale auth chatgpt để dùng gói ChatGPT thay cho khóa API. Tính năng thử nghiệm."
    ]
  ],
  modelsLink: "Xem mô hình và nhà cung cấp",
  startHeading: "Bắt đầu sử dụng",
  startLede:
    "Cài đặt Codewhale, kết nối một mô hình và mở thư mục dự án. Bạn có thể thêm plugin và tác nhân khi cần.",
  startGuideLink: "Làm theo hướng dẫn bắt đầu",
  startVocabularyLink: "Xem thuật ngữ sản phẩm",
  chapterAvailability: "Chạy ở đâu",
  availabilityHeading: "Đã có và đang phát triển",
  availabilityLede:
    "Terminal và client trình duyệt cục bộ đã có sẵn. Ứng dụng desktop gốc và ứng dụng web lưu trữ trực tuyến đang được phát triển.",
  availability: [
    [
      "Terminal và trình duyệt cục bộ",
      "Đã phát hành",
      "Cài đặt trên Linux, macOS hoặc Windows, rồi chạy codewhale, hoặc codewhale web để dùng client trình duyệt cục bộ. Bạn cũng có thể dùng npm và Cargo; bản Android trên Termux là bản xem trước."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Có sẵn",
      "Một dự án riêng do cộng đồng duy trì: trò chuyện, chủ đề và thay đổi tệp trong thanh bên VS Code trên cùng Codewhale Runtime. Cài đặt từ VS Code Marketplace.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "Ứng dụng web lưu trữ trực tuyến",
      "Đang phát triển",
      "Đang được xây dựng lại cho khớp với ứng dụng desktop và chưa mở. Để dùng trong trình duyệt ngay hôm nay, hãy chạy codewhale web trên máy của bạn."
    ],
    [
      "Máy tính để bàn",
      "Bản phát triển",
      "Ứng dụng gốc dùng cùng bộ máy. Xem xét và commit thay đổi, theo dõi công việc giữa các dự án, và chỉnh sửa một tin nhắn để gửi lại thành một nhánh mới. Hiện chưa có bản tải xuống công khai."
    ]
  ],
  availabilityNote:
    "Terminal, trình duyệt cục bộ và GUI không cần tài khoản Codewhale. Khi bạn dùng khóa riêng của mình từ nhà cung cấp, nhà cung cấp đó tính phí phần sử dụng này.",
  surfacesHeading: "Làm việc với tệp và công cụ",
  surfaces: [
    [
      "Tệp và terminal",
      "Tạo tệp, chạy lệnh, xem dữ liệu và kiểm thử sản phẩm. Bạn đặt thư mục làm việc và quyền truy cập."
    ],
    [
      "Plugin và ứng dụng đã kết nối",
      "Thêm kỹ năng và công cụ qua plugin và MCP. Xem xét và bật các kết nối bạn muốn tác tử sử dụng."
    ],
    [
      "Trình duyệt và máy tính · xem trước",
      "Dùng công cụ trình duyệt và plugin Computer Use để làm việc trong ứng dụng và website với quyền bạn cấp."
    ],
    [
      "Phiên có thể tiếp tục",
      "Giữ cuộc trò chuyện, kết quả công cụ và lịch sử công việc cùng nhau. Tiếp tục tác vụ trong terminal hoặc client trình duyệt cục bộ."
    ],
    [
      "Đội tác tử",
      "Dùng Fleet chia công việc lớn cho tác tử có vai trò và mô hình khác nhau, rồi theo dõi tiến độ ở một nơi."
    ]
  ],
  runtimeLink: "Khám phá công cụ và tích hợp",
  installBandHeading: "Cài đặt trên macOS hoặc Linux",
  copy: "Sao chép",
  copied: "Đã sao chép ✓",
  binaries: "Bản nhị phân",
  chinaMirrors: "Mirror Trung Quốc",
  installGuideLink: "Đọc hướng dẫn cài đặt",
  communityHeading: "Đóng góp cho Codewhale",
  communityBody:
    "{count} người đã góp phần xây dựng Codewhale. Báo lỗi, cải thiện tài liệu hoặc đóng góp mã trên GitHub.",
  communityLinksAria: "Liên kết cộng đồng",
  contribute: "Đóng góp trên GitHub",
};
