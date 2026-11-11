import type { ComputerUseDict } from "../types";

/**
 * Indonesian dictionary for `app/[locale]/computer-use/page.tsx` and the
 * Computer Use section of the install page. Product names, the menu items
 * Pause, Stop and Check for updates, and the macOS setting names stay as
 * the app shows them; formal "Anda" address matches the rest of the site.
 */
export const computerUse: ComputerUseDict = {
  metaTitle: "Computer Use · Pratinjau · Codewhale",
  metaDescription: "Unduh dan siapkan Codewhale Computer Use untuk Mac. Kendali aplikasi di latar belakang, pengaturan izin, serta kontrol Pause dan Stop di tangan Anda.",
  title: "Computer Use",
  lead: "Biarkan agen Anda bekerja di aplikasi Mac sementara Anda tetap menggunakan komputer.",
  publisher: "Oleh Codewhale",
  download: "Unduh untuk Mac",
  downloadZip: "Arsip ZIP (digunakan oleh pembaru dalam aplikasi)",
  requirements: "macOS 13.5 atau lebih baru · Apple silicon dan Intel",
  included: "Cukup satu unduhan aplikasi. Tidak perlu memasang Node atau kompiler secara terpisah.",
  pendingTitle: "Unduhan Mac sedang disiapkan",
  pendingBody: "Pemasang publik akan muncul di sini setelah notarisasi Apple dan pemeriksaan rilis selesai.",
  unavailableTitle: "Ketersediaan unduhan tidak dapat diperiksa",
  unavailableBody: "Muat ulang halaman ini untuk mencoba lagi, atau lihat rilis yang sudah diterbitkan di bawah.",
  releases: "Rilis yang diterbitkan",
  receipt: "Detail verifikasi unduhan",
  setup: "Siapkan Mac Anda",
  steps: [
    { title: "Instal aplikasi", body: "Buka disk image, lalu seret Codewhale Computer Use ke folder “Aplikasi”. Buka aplikasinya dari “Aplikasi”, kemudian pilih Computer Use dari ikon paus di bar menu Anda." },
    { title: "Tinjau izin", body: "Gunakan tombol penyiapan untuk membuka “Aksesibilitas” dan “Perekaman Layar” di Pengaturan Sistem. Anda yang menentukan izin mana yang diberikan." },
    { title: "Hubungkan agen Anda", body: "Pilih pengaturan Codewhale di bawah yang sesuai dengan versi CLI Anda. Host MCP lain yang kompatibel dapat mengikuti panduan pengaturan di repositori." },
  ],
  controlsTitle: "Kendali tetap di tangan Anda",
  controlsBody: "Setelah izin macOS diberikan, mode standar bekerja tanpa meminta izin untuk setiap aplikasi. Aplikasi yang Anda blokir tetap tidak dapat diakses; Pause menjeda penggunaan komputer dan Stop computer use mengakhiri sesi saat ini. Persetujuan ketat bersifat opsional. Minta pratinjau langsung saat Anda ingin melihat prosesnya.",
  updateTitle: "Perbarui saat Anda mau",
  updateBody: "Pilih Check for updates di aplikasi. Sebelum memasang pembaruan, aplikasi memeriksa berkas unduhan, tanda tangan Codewhale, dan notarisasi Apple, serta menyimpan versi sebelumnya untuk pemulihan.",
  help: "Penyiapan dan pemecahan masalah",
  notes: "Catatan rilis",
  demo: "Lihat pemeriksaan latar belakang",
  source: "Lihat di GitHub",
  platforms: "Aplikasi pembantu Mac masih beta dan plugin katalog hanya mendukung Mac. Rilis yang diterbitkan juga menyediakan pratinjau eksperimental tanpa tanda tangan untuk Windows x64; Linux memerlukan penyiapan eksperimental dari kode sumber atau Docker. Input pada Windows dan Linux menggunakan desktop bersama.",
  connectTitle: "Hubungkan Codewhale",
  connectVersion: "Codewhale 0.10.2 menyertakan plugin Computer Use versi lama. Memasang entri katalog tidak memperbaruinya. Hubungkan langsung aplikasi pembantu Mac yang terpasang melalui MCP.",
  connectLimit: "Koneksi MCP langsung menambahkan alat tanpa keterampilan plugin. Jangan gunakan bersamaan dengan plugin bawaan lama. Jika plugin itu aktif, jalankan /plugin disable computer-use terlebih dahulu.",
  connectRuntime: "Aplikasi sudah menyertakan Node; tidak perlu pemasangan terpisah. Jika terpasang di lokasi lain, isi APP dengan jalur bundel aplikasi yang sebenarnya.",
  connectReload: "Dalam sesi terminal yang sedang berjalan, jalankan /mcp reload, lalu minta:",
  connectPrompt: "Periksa izin Computer Use dengan request_access. Jangan kirim input.",
  connectExpected: "Periksa via: app, versi aplikasi pembantu, dan izin yang belum diberikan dalam respons. Jika aplikasi pembantu tidak ditemukan, berhenti dan ikuti petunjuk pengaturan. Ini hanya memeriksa koneksi dan izin.",
  connectGuide: "Panduan pengaturan MCP",
  contributeTitle: "Bantu tingkatkan Computer Use",
  contributeBody: "Perbaiki dokumentasi atau uji alur kerja aplikasi dan bagikan hasil yang dapat direproduksi.",
  contributeGuide: "Panduan kontribusi",
  contributeIssue: "Laporkan masalah",
};
