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
  lead: "Biarkan Codewhale bekerja di aplikasi Anda sementara Anda tetap bekerja. Beri izin sekali di Mac ini dan hentikan penggunaan komputer kapan saja.",
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
    { title: "Aktifkan Computer Use", body: "Di pengaturan Connectors Codewhale, instal atau perbarui Computer Use, lalu tinjau dan aktifkan. Minta Codewhale bekerja di sebuah aplikasi. Saat pertama kali digunakan, pilih Trust until revoked di pembantu native; kepercayaan ini tetap berlaku saat berganti aplikasi, model, dan tugas." },
  ],
  controlsTitle: "Kendali tetap di tangan Anda",
  controlsBody: "Kontrol ringkas Codewhale muncul di samping aplikasi yang digunakan. Pause menjeda penggunaan komputer; Stop computer use mengakhiri semua sesi komputer saat ini. Pilih Turn off di pembantu untuk mencabut akses Codewhale. Minta pratinjau langsung saat Anda ingin melihat prosesnya.",
  updateTitle: "Perbarui saat Anda mau",
  updateBody: "Pilih Check for updates di aplikasi. Sebelum memasang pembaruan, aplikasi memeriksa berkas unduhan, tanda tangan Codewhale, dan notarisasi Apple, serta menyimpan versi sebelumnya untuk pemulihan.",
  help: "Penyiapan dan pemecahan masalah",
  notes: "Catatan rilis",
  demo: "Lihat pemeriksaan latar belakang",
  source: "Kode sumber dan platform lain",
  platforms: "Unduhan ini untuk Mac. Windows dan Linux saat ini menggunakan plugin dari kode sumber dan penyiapan di sisi host.",
};
