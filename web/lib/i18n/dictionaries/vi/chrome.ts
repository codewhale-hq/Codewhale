import type { ChromeDict } from "../types";

/**
 * Vietnamese chrome pack. Native rewrite mirroring the current English copy —
 * the "any model, on your machine" wordmark tag and the deep-dive tagline;
 * no trace of the old positioning. Terminology follows the TUI locale pack
 * (`crates/tui/locales/vi.json`) so the site and the runtime name the same
 * things the same way: nhà cung cấp (provider), phiên (session), kho mã
 * (repository), mức quyền (permission posture), biên nhận (receipt). Product
 * terms — Codewhale, Plan / Work / Operate, Ask / Auto-Review / Full Access,
 * fleet, Workflow, Runtime — stay literal, as they do in the TUI.
 */
export const chrome: ChromeDict = {
  navDocs: "Tài liệu",
  // "Câu hỏi thường gặp" is the full form; the horizontal masthead nav needs
  // the short one. The footer, which stacks vertically, keeps the full form.
  navContribute: "Đóng góp",
  navSignIn: "Đăng nhập",
  navDesktop: "Máy tính để bàn",

  navFeatures: "Khả năng",
  navModels: "Mô hình",
  navPlugins: "Plugin",

  skipToContent: "Bỏ qua tới nội dung chính",

  navPrimaryAria: "Điều hướng chính",
  navHomeAria: "Trang chủ Codewhale",

  installCta: "Cài đặt →",

  dateLocale: "vi-VN",

  menuOpen: "Mở menu",
  menuClose: "Đóng menu",
  translationMachineDraft: "Bản dịch này là bản nháp do máy dịch và chưa được người bản ngữ xem lại.",

  themeAuto: "tự động",
  themeLight: "sáng",
  themeDark: "tối",
  themeAria: "Giao diện: {mode} (nhấn để đổi)",
  themeTitle: "Giao diện · tự động / sáng / tối",

  footerProduct: "Sản phẩm",
  footerProject: "Dự án",
  footerDocs: "Tài liệu",
  footerGuide: "Hướng dẫn bắt đầu",
  footerInstall: "Cài đặt",
  footerModels: "Mô hình",
  footerFaq: "Câu hỏi thường gặp",
  // The GitHub tracker, named as GitHub names it — the same choice id/ja/pt-BR made.
  footerContribute: "Đóng góp",
  footerRoadmap: "Lộ trình",
  footerLicense: "Giấy phép MIT",
  footerMerch: "Hàng lưu niệm",
  footerTerms: "Điều khoản dịch vụ",
  footerPrivacy: "Quyền riêng tư",
  footerChangelog: "Nhật ký thay đổi",
  footerCanonicalSource: "Nguồn chính thức: ",
  footerReleases: " · Bản phát hành: ",
  footerReleasesLink: "Bản phát hành trên GitHub",
  footerSecurity: "Bảo mật",
  footerLegal: "Pháp lý",

  switcherLabel: "Ngôn ngữ",
  switcherSwitchTo: "Chuyển sang {label}",
  partialBadge: "(một phần)",

  stateReleased: "Đã phát hành",
  stateAvailable: "Có sẵn",
  stateBeta: "Beta",
  stateDev: "Bản dựng phát triển",
  stateOpen: "Đang mở",
  stateInviteOnly: "Chỉ theo lời mời",
  stateNotOpen: "Chưa mở",
  stateNotPublic: "Chưa công khai",
  stateInSource: "Có trong mã nguồn, chưa phát hành",
  numbersAsOf: "Tính đến {date}",
  downloadMacos: "Tải xuống cho macOS",
  downloadVersion: "Phiên bản {version}",
  downloadSize: "Kích thước {size}",
  downloadChecksum: "Mã kiểm tra SHA-256",
  downloadSigned: "Đã ký và được công chứng",
};
