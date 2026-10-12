<!-- source: README.md sha256:75bb6a506af5 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-inverted.svg">
  <img src="brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Agen pengodean sumber terbuka yang bekerja dengan model apa pun.**

Codewhale membaca proyek Anda, mengedit berkas, menjalankan perintah, dan
memeriksa pekerjaannya sendiri, di terminal Anda, dengan model hosted atau lokal
pilihan Anda.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Situs web](https://codewhale.net) · [Dokumentasi](docs/README.md) · [Changelog](CHANGELOG.md) · [Berkontribusi](CONTRIBUTING.md)

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="web/public/codewhale-tui-0102-demo.png" alt="Sesi terminal Codewhale" width="760">

<sub>Tangkapan terminal Codewhale sungguhan dari sesi demo lokal yang sudah dikonfigurasi.</sub>

</div>

## Instalasi

macOS dan Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Installer mengunduh biner yang checksum-nya sudah diverifikasi ke `~/.local/bin`.
Jika setelah itu `codewhale` menampilkan "command not found", jalankan satu baris
PATH yang dicetak installer, atau lihat
[Menambahkan ke PATH](docs/INSTALL.md#put-it-on-your-path).
Anda dapat memperbarui kapan saja dengan `codewhale update`.

<details>
<summary><b>Windows, npm, Cargo, dan cara lain</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # build from crates.io
```

Docker, Nix, Homebrew di Linux, Android/Termux, unduhan manual dengan verifikasi
checksum, dan mirror CNB opsional dibahas di
[panduan instalasi](docs/INSTALL.md). Pilih satu cara: beberapa instalasi pada
satu mesin akan saling berebut `PATH`.

</details>

## Mulai cepat

**Kelola kunci API penyedia Anda di satu tempat.** [Buat akun Codewhale](https://app.codewhale.net/register)
atau jalankan `codewhale login` untuk masuk. Simpan dan perbarui kunci API di akun Anda,
lalu gunakan melalui rute model Codewhale pada perangkat yang sudah masuk.
[Siapkan kunci akun](docs/CONFIGURATION.md#account-provider-keys).
Masuk tidak mengunggah kunci lokal yang sudah ada; penggunaan lokal tetap tersedia tanpa akun.

1. **Buka proyek Anda.** Jalankan `codewhale` di folder yang ingin Anda kerjakan.
2. **Hubungkan model.** Jalankan `/provider` (atau tekan `F3`) untuk menambahkan
   kunci model hosted atau memilih runtime lokal. Jika Ollama sudah berjalan
   dengan model obrolan, Codewhale beralih ke model itu secara otomatis. Gunakan
   `/model` untuk mengganti model.
3. **Berikan tugas yang konkret.**

```text
Fix the failing tests and explain what changed.
```

Tugas yang sama dapat dijalankan secara headless dari skrip atau job CI:

```bash
codewhale exec --auto "fix the failing tests and explain what changed"
```

Jalankan `/help` untuk melihat perintah dan pintasan keyboard.

## Cara menjalankannya

Setiap klien mengendalikan Codewhale Runtime lokal yang sama, sehingga sesi,
alat, dan izin berperilaku sama di mana pun.

| Perintah | Fungsinya |
| --- | --- |
| `codewhale` | Antarmuka terminal interaktif |
| `codewhale exec "…"` | Satu giliran headless dari skrip atau CI; `--auto` mengizinkan alat berkas dan shell, `--output-format stream-json` men-streaming JSON |
| `codewhale web` | [Klien peramban lokal](docs/WEB.md) bawaan di `127.0.0.1` |
| `codewhale review --pr N` | [Tinjauan pull request](docs/GITHUB_ACTION.md) yang bersifat saran; pengiriman hasilnya opsional |
| Runtime API | [API HTTP lokal](docs/RUNTIME_API.md) untuk thread, event, dan persetujuan |

Aplikasi desktop native (GPUI) sedang dibangun sebagai klien produk untuk akun
yang masuk; lihat ketersediaannya di
[halaman produk](https://codewhale.net/en/product).
[Ekstensi VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
yang dipelihara komunitas terhubung ke Runtime yang sama dari sidebar
([sumber](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Apa yang bisa dilakukan

- **Model apa pun, tanpa terkunci.** Lebih dari 40 rute penyedia bawaan,
  termasuk Anthropic, DeepSeek, Google, Mistral, Moonshot, OpenAI, OpenRouter,
  xAI, dan lainnya, ditambah endpoint apa pun yang kompatibel dengan OpenAI dan
  model lokal melalui Ollama, vLLM, atau SGLang. [Penyedia](docs/PROVIDERS.md)
- **Kendali tetap di tangan Anda.** Mode Plan menelusuri tanpa mengubah apa pun;
  Work dan Operate membuat perubahan. Posture persetujuan menentukan kapan
  pemanggilan alat memerlukan persetujuan Anda, `/undo` dan `/restore`
  memulihkan perubahan di workspace, dan `/receipts` mencantumkan setiap berkas,
  perintah, dan persetujuan dalam satu sesi. [Mode](docs/MODES.md) ·
  [Receipt](docs/RECEIPTS.md)
- **Dibuat untuk pekerjaan panjang.** Tetapkan `/goal` yang tahan lama,
  delegasikan pekerjaan terbatas ke [sub-agen](docs/SUBAGENTS.md), jalankan
  [tim agen](docs/FLEET.md) yang diawasi dengan pemeriksaan biaya sebelum
  berjalan, atau buat skripnya sebagai [workflow](docs/WORKFLOW_AUTHORING.md) yang
  disimpan di repositori.
- **Perluas yang sudah Anda gunakan.** Hubungkan [server MCP](docs/MCP.md), pasang
  [skill](docs/SKILLS.md) dan [plugin](docs/PLUGINS.md), jalankan
  [hook](docs/HOOKS.md) pada event sesi dan alat, serta muat
  [plugin Claude Code](docs/CLAUDE_PLUGIN_COMPAT.md) yang sudah ada.
- **Computer Use.** Plugin bawaan menambahkan alat untuk mengamati dan
  mengoperasikan aplikasi lain. Tinjau akses yang diminta dan aktifkan sebelum
  digunakan. [Panduan](crates/tui/plugins/computer-use/README.md)

## Mode dan izin

| | Pilih dengan | Opsi |
| --- | --- | --- |
| **Mode** — apa yang sedang dikerjakan agen | `Tab` atau `/mode` | Plan (menelusuri, tanpa perubahan) · Work (mengedit dan menjalankan) · Operate (menuntaskan tujuan lewat langkah yang direncanakan dan diverifikasi) |
| **Posture** — kapan agen bertanya lebih dulu | `Shift+Tab` | Ask · Auto-Review · Full Access |

Full Access tetap menghormati batasan kebijakan yang bersifat mutlak.
[Panduan mode dan izin](docs/MODES.md) menjelaskan setiap opsi.

## Keamanan

Codewhale berjalan di mesin Anda dengan akses yang Anda berikan. Posture
persetujuan dan aturan repositori membatasi apa yang boleh dilakukan agen, dan
perintah dijalankan di dalam sandbox OS jika didukung (Seatbelt di macOS;
bubblewrap di Linux jika terpasang dan berfungsi). `/preview-request` menampilkan permintaan
persis yang sudah disamarkan sebelum apa pun dikirim. Harga model yang tidak
diketahui tetap dicatat sebagai tidak diketahui, bukan dilaporkan gratis.

Lihat [urutan otorisasi](docs/AUTHORIZATION_ORDER.md),
[sandbox](docs/SANDBOX.md), dan [telemetri](docs/TELEMETRY.md): penghitungan
penggunaan aktif secara default dan `codewhale config set telemetry false`
menonaktifkannya.

## Dokumentasi

| Mulai dari sini | Pelajari lebih dalam |
| --- | --- |
| [Instalasi](docs/INSTALL.md) | [Konfigurasi](docs/CONFIGURATION.md) |
| [Penyedia dan model lokal](docs/PROVIDERS.md) | [Arsitektur](docs/ARCHITECTURE.md) |
| [Mode dan izin](docs/MODES.md) | [Runtime API](docs/RUNTIME_API.md) |
| [Pintasan keyboard](docs/KEYBINDINGS.md) | [Penulisan plugin](docs/PLUGIN_AUTHORING.md) |
| [Tinjauan PR GitHub](docs/GITHUB_ACTION.md) | [Seluruh dokumentasi](docs/README.md) |

## Komunitas

Laporan bug, ide fitur, dan pull request sangat disambut, baik Anda sudah
berbulan-bulan memakai Codewhale maupun baru mencobanya. Jika ada penyedia yang
belum ada atau alur kerja yang terasa canggung,
[buka issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) atau
[kirim pull request](CONTRIBUTING.md). Kontribusi pertama disambut, dan
kontributor tetap mendapat kredit atas pekerjaan yang digabungkan.
[Struktur repositori](CONTRIBUTING.md#project-structure) adalah tempat yang baik
untuk memulai.

Bergabunglah di [Discord](https://discord.gg/37gfS3ksug), atau tambahkan Hunter
di WeChat (`hunterbown`) dan minta bergabung ke grup Whale Brothers.

## Sejarah dan lisensi

Codewhale berawal dari `deepseek-tui` dan masih membaca konfigurasi serta sesi
proyek itu. Kini proyek ini netral terhadap penyedia, dipelihara secara mandiri,
dan tidak berafiliasi dengan penyedia model mana pun. Terima kasih kepada
[setiap kontributor](docs/CONTRIBUTORS.md) dan komunitas sumber terbuka yang
membantunya berkembang.

[MIT](LICENSE). Bagian yang diadaptasi dari proyek sumber terbuka lain dicatat di
[pemberitahuan pihak ketiga](docs/THIRD_PARTY_NOTICES.md).
