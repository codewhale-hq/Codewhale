<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Agen pengodean sumber terbuka untuk terminal Anda yang bekerja dengan model apa pun.**

Berikan tugas kepada Codewhale, lalu ia membaca repositori, mengedit berkas, dan
menjalankan perintah sampai tugas selesai atau ia memerlukan masukan dari Anda.
Codewhale bekerja dengan lebih dari 40 penyedia bawaan, DeepSeek V4.1 Flash
secara default, dan dengan model yang berjalan di mesin Anda sendiri. `/receipts` mencantumkan
berkas yang diubahnya, perintah yang dijalankannya, serta panggilan web dan MCP
yang dilakukannya.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Situs web](https://codewhale.net) · [Dokumentasi](../README.md) · [Changelog](../../CHANGELOG.md) · [Berkontribusi](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Sesi terminal Codewhale" width="760">

<sub>Build pengembangan 0.10.2 dengan model uji yang berjalan sesuai skrip, dalam Akses penuh di dalam workspace sekali pakai. Edit, tiga pengujian, dan tinjauan sub-agen benar-benar dijalankan.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — tambahan 30% penggunaan DeepSeek v4.1 Flash" width="760">
  </a>
</p>

[Lihat penawaran Codewhale × OrcaRouter](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale menerima komisi rujukan dari OrcaRouter.

## Instalasi

macOS dan Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Installer menaruh `codewhale` dan alias pendeknya `codew`, yang checksum-nya
sudah diverifikasi, di `~/.local/bin`. Jika setelah itu shell Anda menampilkan
`command not found`, jalankan baris PATH yang dicetak installer
([detail](../INSTALL.md#put-it-on-your-path)). `codewhale update`
memperbaruinya. Perintah-perintah ini memasang rilis terbaru. Halaman ini
mengikuti `main`, jadi fitur yang ditandai dengan versi akan hadir di rilis
tersebut; [changelog](../../CHANGELOG.md) mencantumkan apa yang ditambahkan setiap
rilis.

<details>
<summary><b>Windows, npm, Cargo, dan cara instalasi lainnya</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

Biner siap pakai tersedia untuk Linux, macOS, dan Windows di x64 dan arm64. Nix,
Homebrew di Linux, Android/Termux (pratinjau), dan unduhan manual dibahas di
[panduan instalasi](../INSTALL.md), sedangkan container di
[Docker](../DOCKER.md). Gunakan satu cara instalasi per mesin; jika tidak, dua
salinan akan ada di `PATH` dan yang muncul lebih dulu yang dipakai.

</details>

## Mulai cepat

1. Commit atau stash pekerjaan Anda, lalu jalankan `codewhale` dari root
   repositori git. Secara default, Codewhale menerapkan edit berkas di sana
   tanpa bertanya dan menampilkan diff-nya kepada Anda.
2. Jalankan `/provider` (atau tekan `F3`), pilih penyedia, tempel kunci API-nya,
   lalu pilih model. DeepSeek adalah penyedia default. Jika tidak punya kunci,
   pilih runtime lokal. Jika belum ada penyedia yang dikonfigurasi dan Ollama
   sudah menyajikan model obrolan, Codewhale memakainya secara otomatis; mulai
   0.10.2, Codewhale menawarkannya saat Anda mengirim pesan pertama.
3. Ketik tugas:

```text
Fix the failing tests and explain what changed.
```

Perintah baca-saja langsung dijalankan. Untuk perintah shell lainnya, Codewhale
bertanya lebih dulu: `y` menjalankannya sekali, `a` mengizinkan perintah serupa
selama sisa percakapan, `n` menolaknya. `/undo` membatalkan permintaan terakhir
Anda beserta perubahan berkasnya. `/help` menampilkan daftar perintah dan tombol.

Untuk skrip dan CI, `codewhale exec` menjalankan satu tugas tanpa antarmuka.
Secara default, `--auto` memakai Akses penuh: alat berjalan tanpa bertanya,
perintah shell tidak dibatasi sandbox Codewhale, dan mode kepercayaan
mengizinkan alat berkas menulis di luar workspace. Tambahkan
`--sandbox workspace-write` untuk mempertahankan sandbox shell dan batasan
workspace bagi alat berkas, dengan mode kepercayaan dinonaktifkan:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## Mode dan izin

Instalasi baru dimulai dalam mode Work dengan izin Tanya.

| | Ganti dengan | Opsi |
| --- | --- | --- |
| Mode | `Tab` (saat input kosong) atau `/mode` | **Plan** membaca dan mencari tetapi tidak mengubah apa pun. **Work** mengedit berkas dan menjalankan perintah. **Operate** memiliki akses yang sama dengan Work, memperlakukan permintaan Anda sebagai tujuan, dan terus berjalan sampai tujuan itu terverifikasi atau ia memerlukan masukan dari Anda. |
| Izin | `Shift+Tab` | **Tanya** (Ask) menerapkan edit berkas di repositori Anda tanpa bertanya dan bertanya sebelum perintah shell yang bukan baca-saja. **Tinjauan otomatis** (Auto-Review) membiarkan aturan dan model peninjau menyetujui panggilan rutin dan menolak panggilan berisiko tinggi; beralihlah ke Tanya untuk menyetujuinya sendiri. **Akses penuh** (Full Access) tidak pernah bertanya dan menonaktifkan sandbox Codewhale; aturan repositori dan kebijakan terkelola tetap berlaku. |

Lihat [panduan mode dan izin](../MODES.md) untuk detailnya.

## Apa yang bisa dilakukan

### Baru di 0.10.2

- `/pet on` mengubah terminal Anda menjadi paus animasi yang menunjukkan apa
  yang sedang dilakukan agen, sementara kotak pesan tetap aktif. 980 partikelnya
  mengeja langkah saat ini dalam bahasa Inggris, Tionghoa, atau Jepang.
- `/workbar terminal` menampilkan shell yang dijalankan agen selagi shell itu
  berjalan. Ketik ke salah satunya, atau tekan `Ctrl+N` untuk membuka shell Anda
  sendiri (macOS dan Linux).
- `/undo` membatalkan seluruh permintaan terakhir Anda dalam satu langkah,
  termasuk setiap berkas yang diubahnya, dan `/diff` berfungsi di folder tanpa
  Git.
- Saat sebuah rencana sudah siap, Codewhale
  bertanya apakah akan memulai Work dengan Tanya atau Tinjauan otomatis, atau
  melanjutkan perencanaan.
- `/color` memberi setiap obrolan warna kotak penulisan sendiri, yang disimpan
  bersama sesi.
- `web_search` dapat memakai Search API baru dari DeepSeek: atur
  `[search] provider = "deepseek"` dengan kunci DeepSeek.
- Di luar Akses penuh, `curl` atau `wget` yang akan mengunggah kredensial tidak
  pernah berjalan tanpa persetujuan Anda, dan perintah yang menyembunyikan
  Unicode tak terlihat akan ditolak. `write` tidak lagi menimpa berkas yang belum
  dibaca agen, atau berkas yang berubah setelah agen membacanya.

### Model

Lebih dari 40 penyedia sudah tersedia bawaan, termasuk Anthropic, OpenAI,
DeepSeek, Google, Mistral, Moonshot, OpenRouter, dan xAI. Endpoint apa pun yang
kompatibel dengan OpenAI juga bisa dipakai, begitu pula model lokal melalui
Ollama, vLLM, atau SGLang. Defaultnya adalah `deepseek-flash` dari DeepSeek,
yang dipindahkan DeepSeek ke DeepSeek-V4.1-Flash-1017 pada 2026-10-17; 0.10.1
menjalankannya tanpa pembaruan
([detail](../PROVIDERS.md#deepseek-v41-flash)).

`/model` mengganti model di antara giliran, dan `/model auto` memilih model
untuk setiap giliran (`/router` mengatur caranya). `/turn inspect` menampilkan
model yang dipakai sebuah giliran, biayanya, dan berapa lama prosesnya.
`codewhale auth chatgpt` memungkinkan Anda memakai paket ChatGPT sebagai ganti
kunci API (eksperimental). Lihat [Penyedia](../PROVIDERS.md).

### Receipt dan pembatalan

`/receipts`, atau `codewhale receipts --last` setelah Anda keluar, mencantumkan
berkas yang diubah, perintah yang dijalankan, panggilan web dan MCP, agen yang
dimulai, persetujuan beserta siapa yang memberikannya, dan kegagalan. `/undo` membatalkan permintaan terakhir Anda beserta berkas yang diubahnya, dalam satu langkah dan tanpa mode kepercayaan. `/restore`
mengembalikan seluruh workspace
ke snapshot sebelumnya; karena itu menimpa edit yang lebih baru, perintah ini
memerlukan `/trust on` atau Akses penuh. Lihat [Receipt](../RECEIPTS.md).

Eksekusi exec yang menyediakan alat disimpan dalam setiap format output, jadi
`codewhale receipts --last` bisa dipakai setelah `exec --auto`, termasuk dengan
`--json`. Eksekusi yang hanya menjawab dengan teks atau `--json` hanya disimpan
saat melanjutkan percakapan atau ketika pemadatan percakapan dimulai; gunakan
`--output-format stream-json` agar selalu tersimpan untuk receipt atau
`exec --continue`.

### Pekerjaan yang lebih panjang

`/goal` menetapkan sasaran yang berlanjut lintas giliran. Codewhale dapat
membagi pekerjaan ke beberapa [sub-agen](../SUBAGENTS.md), menjalankan
[armada](../FLEET.md) agen, atau mengikuti
[workflow](../WORKFLOW_AUTHORING.md) yang disimpan di repositori.

### Beralih dari agen lain

Codewhale membaca `AGENTS.md` dengan sendirinya. Berkas instruksi yang ditulis
untuk alat lain, seperti `CLAUDE.md`, `.cursorrules`, atau
`.github/copilot-instructions.md`, hanya dimuat jika Anda mengaktifkannya dengan
`project_instruction_imports = ["claude"]` (atau `"cursor"`, `"cline"`,
`"windsurf"`, `"copilot"`, `"all"`) di config Anda; lihat
[config.example.toml](../../config.example.toml). Codewhale memberi peringatan jika
menemukan berkas seperti itu yang tidak dimuatnya.

`/import-claude` menyiapkan impor pengaturan Claude Code dan server MCP Anda
untuk Anda tinjau. `/plugin install` menerima plugin Claude Code yang berisi
skill, perintah, profil agen, dan server MCP, dan menolak plugin yang memerlukan
hook atau server LSP ([detail](../CLAUDE_PLUGIN_COMPAT.md)).

### Ekstensi

Hubungkan [server MCP](../MCP.md), tambahkan [skill](../SKILLS.md) dan
[plugin](../PLUGINS.md), dan jalankan [hook](../HOOKS.md) pada event sesi
dan alat. Mode kode (eksperimental, aktif secara default) memungkinkan model
menulis satu program `execute_tools` yang memanggil beberapa alat MCP dan
plugin; setiap panggilan tetap tunduk pada aturan persetujuan Anda dan tetap
memiliki receipt-nya. Plugin Computer Use yang dipasang terpisah (eksperimental, hanya macOS) dapat
mengamati dan mengoperasikan aplikasi lain. Plugin ini tetap nonaktif sampai
Anda meninjau aksesnya dan mengaktifkannya
([panduan](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## Cara menjalankannya

| Perintah | Fungsinya |
| --- | --- |
| `codewhale` | Aplikasi terminal interaktif |
| `codewhale exec "…"` | Satu tugas tanpa antarmuka. Tanpa flag tambahan, ia menjawab tanpa memakai alat; `--auto` mengaktifkan alat dengan Akses penuh secara default, `--sandbox workspace-write` membatasinya, dan `--output-format stream-json` men-streaming event sebagai JSON |
| `codewhale web` | UI web lokal di `127.0.0.1` ([panduan](../WEB.md)) |
| `codewhale review --pr N` | Tinjauan yang bersifat saran untuk pull request GitHub. [Penyiapan bot GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) menghasilkan workflow dengan versi dipasangi pin untuk tinjauan otomatis dan `@codewhale review`, memakai kunci modelmu sendiri dan identitas App opsional |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md), API HTTP lokal untuk thread, event, persetujuan, dan perubahan berkas setiap giliran |
| `codewhale serve --acp` | Agent Client Protocol melalui stdio, untuk editor seperti Zed |

Aplikasi terminal, `exec`, `web`, dan Runtime API memakai satu engine yang sama
([arsitektur](../ARCHITECTURE.md)).
[Ekstensi VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
yang dipelihara komunitas terhubung ke engine itu dari sidebar
([sumber](https://github.com/HengQuWorld/CodeWhale-VSCode)).
Aplikasi desktop native yang dibangun di atas Runtime API sedang dikembangkan;
belum ada unduhan publik.

## Biaya dan akun

Codewhale gratis dan berlisensi MIT. Dengan kunci API sendiri, Anda membayar
langsung ke penyedia; model lokal tidak memerlukan kunci. Footer dan `/cost`
menampilkan biaya sesi jika harganya diketahui, dan harga yang tidak diketahui
ditampilkan sebagai tidak diketahui, tidak pernah sebagai gratis.

Anda tidak memerlukan akun. Kunci yang Anda tambahkan dengan `/provider` tetap
tersimpan di mesin Anda.

## Keamanan dan privasi

Dalam Tanya dan Tinjauan otomatis, perintah berjalan di sandbox OS jika sistem
menyediakannya: Seatbelt di macOS, dan bubblewrap di Linux jika terpasang.
Windows belum memiliki sandbox OS, dan Akses penuh menonaktifkan sandbox. Di
dalam sandbox, perintah shell tidak dapat membaca penyimpanan kredensial seperti
`~/.ssh`, `~/.aws`, dan berkas `.env`; mulai 0.10.2, aturan yang sama berlaku
untuk sesi terminal dan lebih banyak alat berkas. `/preview-request` menampilkan pratinjau permintaan berikutnya yang sudah
disamarkan tanpa mengirimnya.

Telemetri aktif secara default. Telemetri mengirim hitungan agregat (versi dan
platform, durasi dan hasil sesi, penyedia bawaan yang Anda pakai, hitungan fitur
dan error) bersama ID instalasi acak yang diganti setiap 90 hari, dan tidak
pernah mengirim percakapan, kode, prompt, nama model, nama berkas atau
repositori, maupun kredensial.
`codewhale config set telemetry false` menonaktifkannya. Saat dijalankan,
Codewhale juga memeriksa GitHub untuk rilis baru; atur
`CODEWHALE_NO_UPDATE_CHECK=1` untuk melewatinya. Selengkapnya di
[urutan otorisasi](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md),
dan [telemetri](../TELEMETRY.md).

## Dokumentasi

| Mulai dari sini | Referensi |
| --- | --- |
| [Instalasi](../INSTALL.md) | [Konfigurasi](../CONFIGURATION.md) |
| [Penyedia dan model lokal](../PROVIDERS.md) | [Arsitektur](../ARCHITECTURE.md) |
| [Mode dan izin](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [Pintasan keyboard](../KEYBINDINGS.md) | [Penulisan plugin](../PLUGIN_AUTHORING.md) |
| [Tinjauan PR GitHub](../GITHUB_ACTION.md) | [Seluruh dokumentasi](../README.md) |

## Komunitas

Jika ada penyedia yang belum tersedia atau alur kerja yang terasa canggung,
[buka issue](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) atau
[kirim pull request](../../CONTRIBUTING.md). Kontributor tetap mendapat kredit atas
pekerjaan yang digabungkan, dan
[struktur repositori](../../CONTRIBUTING.md#project-structure) menunjukkan letak
setiap bagian.

Ikuti obrolan di [Discord](https://discord.gg/37gfS3ksug), atau tambahkan
maintainer, Hunter, di WeChat (`hunterbown`) dan minta bergabung ke grup
Whale Brothers.

## Sejarah dan lisensi

Codewhale berawal dari `deepseek-tui` dan masih membaca konfigurasi serta sesi
proyek itu. Codewhale dipelihara secara mandiri dan tidak berafiliasi dengan
DeepSeek atau penyedia model lain mana pun. Terima kasih kepada
[semua yang telah berkontribusi](../CONTRIBUTORS.md).

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE). Kode yang diadaptasi dari proyek sumber terbuka lain dicantumkan
di [pemberitahuan pihak ketiga](../THIRD_PARTY_NOTICES.md).
