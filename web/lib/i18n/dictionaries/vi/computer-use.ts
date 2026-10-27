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
  lead: "Để Codewhale thao tác trong các ứng dụng trong khi bạn tiếp tục làm việc. Cấp quyền một lần trên máy Mac này và dừng việc sử dụng máy tính bất cứ khi nào bạn muốn.",
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
    { title: "Bật Computer Use", body: "Trong phần cài đặt Connectors của Codewhale, cài đặt hoặc cập nhật Computer Use, rồi xem lại và bật plugin. Yêu cầu Codewhale làm việc trong một ứng dụng. Lần đầu sử dụng, chọn Trust until revoked trong trợ lý native; quyền tin cậy này vẫn có hiệu lực khi đổi ứng dụng, mô hình và tác vụ." },
  ],
  controlsTitle: "Bạn luôn nắm quyền kiểm soát",
  controlsBody: "Nút điều khiển nhỏ gọn của Codewhale xuất hiện cạnh ứng dụng đang được sử dụng. Pause tạm dừng việc sử dụng máy tính; Stop computer use kết thúc tất cả phiên máy tính hiện tại. Chọn Turn off trong trợ lý để thu hồi quyền truy cập của Codewhale. Khi muốn theo dõi công việc, hãy yêu cầu xem trước trực tiếp.",
  updateTitle: "Cập nhật khi bạn muốn",
  updateBody: "Chọn Check for updates (kiểm tra cập nhật) trong ứng dụng. Trước khi cài bản cập nhật, ứng dụng kiểm tra tệp tải về, chữ ký Codewhale và công chứng của Apple, đồng thời giữ lại bản cũ để khôi phục khi cần.",
  help: "Thiết lập và khắc phục sự cố",
  notes: "Ghi chú phát hành",
  demo: "Xem kiểm tra nền",
  source: "Mã nguồn và các nền tảng khác",
  platforms: "Bản tải này dành cho Mac. Windows và Linux hiện dùng plugin từ mã nguồn cùng thiết lập phía máy chủ.",
};
