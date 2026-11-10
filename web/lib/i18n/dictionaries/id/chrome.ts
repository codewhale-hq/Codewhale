import type { ChromeDict } from "../types";

/**
 * Indonesian chrome dictionary — native rewrite mirroring the current English
 * direction. The wordmark tag renders "any model, on your machine" natively;
 * the old positioning is gone.
 *
 * Terminology follows the TUI locale pack (`crates/tui/locales/id.json`):
 * "penyedia" (provider), "izin" / "postur izin" (permission posture),
 * "penalaran" (reasoning), "repositori", "tanda terima" (receipt). The mode
 * names (Plan / Work / Operate) and permission postures (Ask / Auto-Review /
 * Full Access) stay literal there and stay literal here.
 */
export const chrome: ChromeDict = {
  navDocs: "Dokumentasi",
  navStart: "Mulai",
  navInstall: "Instal",
  navFaq: "Tanya jawab",
  navCommunity: "Komunitas",
  navContribute: "Kontribusi",

  navFeatures: "Kemampuan",
  navProduct: "Produk",
  navModels: "Model",
  navPlugins: "Plugin",

  skipToContent: "Lewati ke konten utama",

  navPrimaryAria: "Navigasi utama",
  navHomeAria: "Beranda Codewhale",

  installCta: "Instal →",

  dateLocale: "id-ID",

  menuOpen: "Buka menu",
  menuClose: "Tutup menu",

  themeAuto: "otomatis",
  themeLight: "terang",
  themeDark: "gelap",
  themeAria: "Tema: {mode} (klik untuk mengganti)",
  themeTitle: "Tema · otomatis / terang / gelap",

  footerTagline: "Gunakan model apa pun lewat agen yang bisa Anda baca.",
  footerProduct: "Produk",
  footerProject: "Proyek",
  footerDocs: "Dokumentasi",
  footerGuide: "Panduan memulai",
  footerInstall: "Instalasi",
  footerModels: "Model",
  footerRuntime: "Antarmuka runtime",
  footerFaq: "Tanya jawab",
  footerIssues: "Masalah",
  footerContribute: "Kontribusi",
  footerRoadmap: "Peta jalan",
  footerLicense: "Lisensi MIT",
  footerTerms: "Ketentuan layanan",
  footerPrivacy: "Privasi",
  footerChangelog: "Catatan perubahan",
  footerCanonicalSource: "Sumber kanonis: ",
  footerReleases: " · Rilis: ",
  footerReleasesLink: "Rilis GitHub",
  footerSecurity: "Keamanan",
  footerLegal: "Legal",

  switcherLabel: "Bahasa",
  switcherSwitchTo: "Beralih ke {label}",
  partialBadge: "(sebagian)",
};
