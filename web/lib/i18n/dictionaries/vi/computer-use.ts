import type { ComputerUseDict } from "../types";

/**
 * Vietnamese dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them; the macOS setting and folder names use the Vietnamese
 * system UI names (Trợ năng, Ghi màn hình, Ứng dụng).
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Bản xem trước · Codewhale",
  metaDescription: "Tải và thiết lập Codewhale Computer Use cho Mac. Điều khiển ứng dụng trong nền, thiết lập quyền truy cập, cùng nút Pause và Stop do bạn kiểm soát.",
  title: "Computer Use",
  lead: "Để tác nhân của bạn làm việc trong các ứng dụng Mac trong khi bạn tiếp tục dùng máy tính.",
  publisher: "Từ Codewhale",
  download: "Tải cho Mac",
  downloadZip: "Tệp ZIP (dùng cho trình cập nhật trong ứng dụng)",
  requirements: "macOS 13.5 trở lên · Apple silicon và Intel",
  included: "Chỉ cần tải một ứng dụng. Không cần cài riêng Node hay trình biên dịch.",
  pendingTitle: "Bản tải cho Mac đang được chuẩn bị",
  pendingBody: "Trình cài đặt công khai sẽ xuất hiện ở đây sau khi hoàn tất công chứng của Apple và các bước kiểm tra phát hành.",
  unavailableTitle: "Không kiểm tra được tình trạng tải xuống",
  unavailableBody: "Hãy tải lại trang để thử lại, hoặc xem các bản phát hành bên dưới.",
  releases: "Bản phát hành đã công bố",
  receipt: "Thông tin xác minh bản tải",
  setup: "Thiết lập máy Mac của bạn",
  steps: [
    { title: "Cài đặt ứng dụng", body: "Mở ảnh đĩa và kéo Codewhale Computer Use vào thư mục “Ứng dụng”. Mở ứng dụng từ “Ứng dụng”, rồi chọn Computer Use từ biểu tượng cá voi trên thanh menu." },
    { title: "Xem lại quyền truy cập", body: "Dùng các nút thiết lập để mở “Trợ năng” và “Ghi màn hình” trong Cài đặt Hệ thống. Bạn tự quyết định cấp những quyền nào." },
    { title: "Kết nối tác nhân của bạn", body: "Trong Codewhale, cài Computer Use bằng /plugin marketplace install codewhale computer-use, rồi xem lại và bật plugin. Ứng dụng hỗ trợ được cài riêng. Các host MCP tương thích khác kết nối với ứng dụng theo hướng dẫn thiết lập." },
  ],
  controlsTitle: "Bạn luôn nắm quyền kiểm soát",
  controlsBody: "Sau khi cấp quyền macOS, chế độ tiêu chuẩn hoạt động mà không hỏi quyền cho từng ứng dụng. Các ứng dụng bạn chặn vẫn không thể truy cập; Pause tạm dừng thao tác máy tính và Stop computer use kết thúc các phiên hiện tại. Chế độ phê duyệt nghiêm ngặt là tùy chọn. Yêu cầu xem trước trực tiếp khi bạn muốn theo dõi công việc.",
  updateTitle: "Cập nhật khi bạn muốn",
  updateBody: "Chọn Check for updates (kiểm tra cập nhật) trong ứng dụng. Trước khi cài bản cập nhật, ứng dụng kiểm tra tệp tải về, chữ ký Codewhale và công chứng của Apple, đồng thời giữ lại bản cũ để khôi phục khi cần.",
  help: "Thiết lập và khắc phục sự cố",
  notes: "Ghi chú phát hành",
  demo: "Xem kiểm tra nền",
  source: "Mã nguồn và các nền tảng khác",
  platforms: "Ứng dụng hỗ trợ trên Mac đang ở bản beta và plugin trong danh mục chỉ hỗ trợ Mac. Các bản phát hành cũng có bản xem trước thử nghiệm chưa được ký cho Windows x64; Linux cần thiết lập thử nghiệm từ mã nguồn hoặc qua Docker. Thao tác nhập trên Windows và Linux sử dụng màn hình nền dùng chung.",
};
