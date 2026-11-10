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
  navStart: "Bắt đầu",
  navInstall: "Cài đặt",
  // "Câu hỏi thường gặp" is the full form; the horizontal masthead nav needs
  // the short one. The footer, which stacks vertically, keeps the full form.
  navFaq: "Hỏi đáp",
  navCommunity: "Cộng đồng",
  navContribute: "Đóng góp",

  navFeatures: "Khả năng",
  navProduct: "Sản phẩm",
  navModels: "Mô hình",
  navPlugins: "Plugin",

  skipToContent: "Bỏ qua tới nội dung chính",

  navPrimaryAria: "Điều hướng chính",
  navHomeAria: "Trang chủ Codewhale",

  installCta: "Cài đặt →",

  dateLocale: "vi-VN",

  menuOpen: "Mở menu",
  menuClose: "Đóng menu",

  themeAuto: "tự động",
  themeLight: "sáng",
  themeDark: "tối",
  themeAria: "Giao diện: {mode} (nhấn để đổi)",
  themeTitle: "Giao diện · tự động / sáng / tối",

  footerTagline: "Dùng bất kỳ mô hình nào qua một agent mà bạn đọc được.",
  footerProduct: "Sản phẩm",
  footerProject: "Dự án",
  footerDocs: "Tài liệu",
  footerGuide: "Hướng dẫn bắt đầu",
  footerInstall: "Cài đặt",
  footerModels: "Mô hình",
  footerRuntime: "Runtime",
  footerFaq: "Câu hỏi thường gặp",
  // The GitHub tracker, named as GitHub names it — the same choice id/ja/pt-BR made.
  footerIssues: "Issues",
  footerContribute: "Đóng góp",
  footerRoadmap: "Lộ trình",
  footerLicense: "Giấy phép MIT",
  footerTerms: "Điều khoản dịch vụ",
  footerPrivacy: "Quyền riêng tư",
  footerChangelog: "Nhật ký thay đổi",
  footerCanonicalSource: "Nguồn chính thức: ",
  footerReleases: " · Bản phát hành: ",
  footerReleasesLink: "Bản phát hành trên GitHub",
  footerSecurity: "Bảo mật",

  switcherLabel: "Ngôn ngữ",
  switcherSwitchTo: "Chuyển sang {label}",
  partialBadge: "(một phần)",
};
