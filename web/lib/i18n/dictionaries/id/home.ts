import type { HomeDict } from "../types";

/** Indonesian home copy: useful work with chosen models and connected tools. */

export const home: HomeDict = {
  metaTitle: "Codewhale: agen pemrograman sumber terbuka",
  metaDescription: "Codewhale adalah agen pemrograman sumber terbuka untuk terminal. Baca, edit dan uji proyek dengan model jarak jauh atau lokal pilihan Anda.",
  heroTitle: "Agen pemrograman sumber terbuka untuk model apa pun",
  heroIntro: "{brand} membaca proyekmu, mengedit berkas, dan menjalankan pengujianmu dari terminal. Hubungkan model yang dihosting atau model lokal, lalu pilih tindakan yang memerlukan persetujuanmu.",
  getCodewhale: "Instal Codewhale",
  heroInstallAria: "Perintah instalasi",
  exploreProduct: "Jelajahi Codewhale",
  shotPreview: "Pratinjau terminal",
  screenshotAlt:
    "Codewhale v{version}: tangkapan terminal berisi percakapan, kolom pesan, dan kontrol sesi.",
  latestRelease: "Rilis terbaru {tag}",
  releaseUnavailable: "Status rilis tidak tersedia",
  proofContributors: "{count} kontributor",
  currentSource: "Sumber",
  sourceCandidate: "Belum dirilis",
  publishedRelease: "dirilis",
  gainHeading: "Bangun dan otomatisasikan dari terminalmu",
  gainLede:
    "Jelaskan apa yang ingin kamu buat atau otomatisasikan. Codewhale dapat mengedit berkas, menjalankan perintah, dan memeriksa hasilnya, dengan akses yang kamu kendalikan.",
  gain: [
    [
      "Buat aplikasi dan alat",
      "Buat aplikasi, tambahkan fitur, atau tulis skrip. Codewhale dapat mengerjakan berkas proyek, menjalankan kode, dan menguji hasil yang dibuatnya."
    ],
    [
      "Otomatiskan pekerjaan berulang",
      "Jalankan alur kerja dari terminal, skrip, atau CI. Untuk tugas yang lebih besar, delegasikan sebagian pekerjaan ke Fleet agen dengan model yang berbeda."
    ],
    [
      "Hubungkan alat yang kamu pakai",
      "Tambahkan alat melalui plugin dan server MCP, atau gunakan API dari skripmu sendiri. Setiap layanan memerlukan penyiapan dan autentikasi tersendiri."
    ]
  ],
  chapterModels: "Model Anda",
  modelsHeading: "Gunakan model pilihanmu",
  modelsBody:
    "Hubungkan akun penyedia, endpoint yang kompatibel dengan OpenAI, atau model lokal dan yang kamu host sendiri. Pilih model untuk sesi dan untuk setiap agen dalam Fleet.",
  modelsFacts: [
    [
      "Akun API milikmu",
      "Hubungkan penyedia seperti DeepSeek, OpenAI, Anthropic, atau Google dengan kuncimu sendiri. Model bawaan, deepseek-flash, kini menjalankan DeepSeek-V4.1-Flash."
    ],
    [
      "Gateway milikmu",
      "Gunakan endpoint yang kompatibel dengan OpenAI dan pilih model yang disediakannya."
    ],
    [
      "Inferensi milikmu",
      "Jalankan model lokal atau yang dihosting sendiri dengan Ollama, vLLM, atau SGLang."
    ],
    [
      "Paket ChatGPT milikmu",
      "Masuk dengan codewhale auth chatgpt untuk memakai paket ChatGPT sebagai pengganti kunci API. Eksperimental."
    ]
  ],
  modelsLink: "Lihat model dan penyedia",
  startHeading: "Mulai menggunakan",
  startLede:
    "Instal Codewhale, hubungkan model, dan buka folder proyek. Kamu bisa menambahkan plugin dan agen lain saat membutuhkannya.",
  startGuideLink: "Ikuti panduan memulai",
  startVocabularyLink: "Lihat kosakata produk",
  chapterAvailability: "Tempat menjalankan",
  availabilityHeading: "Tersedia sekarang dan sedang dikembangkan",
  availabilityLede:
    "Terminal dan klien peramban lokal sudah tersedia. Aplikasi desktop native dan aplikasi web yang dihosting sedang dikembangkan ulang.",
  availability: [
    [
      "Terminal dan peramban lokal",
      "Dirilis",
      "Instal di Linux, macOS, atau Windows, lalu jalankan codewhale, atau codewhale web untuk klien peramban lokal. npm dan Cargo juga dapat digunakan; Android di Termux masih berupa pratinjau."
    ],
    [
      "CodeWhale GUI (VS Code)",
      "Tersedia",
      "Proyek terpisah yang dipelihara oleh komunitas: obrolan, utas, dan perubahan berkas di sidebar VS Code di atas Codewhale Runtime yang sama. Pasang dari VS Code Marketplace.",
      "https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode"
    ],
    [
      "Aplikasi web hosted",
      "Dalam pengembangan",
      "Sedang dibangun ulang agar sesuai dengan aplikasi desktop dan belum dibuka. Untuk menggunakannya di peramban sekarang, jalankan codewhale web di komputermu."
    ],
    [
      "Desktop",
      "Build pengembangan",
      "Aplikasi native dengan mesin yang sama. Tinjau dan commit perubahan, ikuti pekerjaan lintas proyek, dan edit pesan untuk mengirimkannya kembali sebagai cabang baru. Belum ada unduhan publik."
    ]
  ],
  availabilityNote:
    "Terminal, peramban lokal, dan GUI tidak memerlukan akun Codewhale. Jika Anda memakai kunci penyedia milik Anda sendiri, penyedia tersebut menagih penggunaan itu.",
  surfacesHeading: "Bekerja dengan berkas dan alat",
  surfaces: [
    [
      "Berkas dan terminal",
      "Buat berkas, jalankan perintah, periksa data, dan uji hasilmu. Kamu menentukan folder kerja dan izin."
    ],
    [
      "Plugin dan aplikasi terhubung",
      "Tambahkan keterampilan dan alat melalui plugin dan MCP. Tinjau dan aktifkan koneksi yang boleh digunakan agen."
    ],
    [
      "Peramban dan komputer · pratinjau",
      "Gunakan alat peramban dan plugin Computer Use untuk bekerja di aplikasi dan situs dengan akses yang kamu berikan."
    ],
    [
      "Sesi yang bisa dilanjutkan",
      "Simpan percakapan, hasil alat, dan riwayat kerja bersama. Lanjutkan tugas di terminal atau klien peramban lokal."
    ],
    [
      "Tim agen",
      "Gunakan Fleet untuk membagi pekerjaan besar ke agen dengan peran dan model berbeda, lalu ikuti kemajuannya di satu tempat."
    ]
  ],
  runtimeLink: "Jelajahi alat dan integrasi",
  installBandHeading: "Instal di macOS atau Linux",
  copy: "Salin",
  copied: "Tersalin ✓",
  binaries: "Biner",
  chinaMirrors: "Mirror Tiongkok",
  installGuideLink: "Baca panduan instalasi",
  communityHeading: "Berkontribusi pada Codewhale",
  communityBody:
    "{count} orang telah ikut membangun Codewhale. Laporkan bug, perbaiki dokumentasi, atau sumbangkan kode di GitHub.",
  communityLinksAria: "Tautan komunitas",
  contribute: "Berkontribusi di GitHub",
};
