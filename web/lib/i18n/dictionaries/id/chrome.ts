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
  navContribute: "Kontribusi",
  navSignIn: "Masuk",
  navDesktop: "Desktop",

  navFeatures: "Kemampuan",
  navModels: "Model",
  navPlugins: "Plugin",

  skipToContent: "Lewati ke konten utama",

  navPrimaryAria: "Navigasi utama",
  navHomeAria: "Beranda Codewhale",

  installCta: "Instal →",

  dateLocale: "id-ID",

  menuOpen: "Buka menu",
  menuClose: "Tutup menu",
  translationMachineDraft: "Terjemahan ini adalah draf mesin yang belum ditinjau oleh penutur asli.",

  themeAuto: "otomatis",
  themeLight: "terang",
  themeDark: "gelap",
  themeAria: "Tema: {mode} (klik untuk mengganti)",
  themeTitle: "Tema · otomatis / terang / gelap",

  footerProduct: "Produk",
  footerProject: "Proyek",
  footerDocs: "Dokumentasi",
  footerGuide: "Panduan memulai",
  footerInstall: "Instalasi",
  footerModels: "Model",
  footerFaq: "Tanya jawab",
  footerContribute: "Kontribusi",
  footerRoadmap: "Peta jalan",
  footerLicense: "Lisensi MIT",
  footerMerch: "Merchandise",
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

  stateReleased: "Dirilis",
  stateAvailable: "Tersedia",
  stateBeta: "Beta",
  stateDev: "Build pengembangan",
  stateOpen: "Terbuka",
  stateInviteOnly: "Hanya undangan",
  stateNotOpen: "Belum dibuka",
  stateNotPublic: "Belum publik",
  stateInSource: "Ada di kode sumber, belum dirilis",
  numbersAsOf: "Per {date}",
  downloadMacos: "Unduh untuk macOS",
  downloadVersion: "Versi {version}",
  downloadSize: "Ukuran {size}",
  downloadChecksum: "Checksum SHA-256",
  downloadSigned: "Ditandatangani dan dinotarisasi",
};
