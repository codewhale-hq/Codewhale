<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>Model boleh milik siapa saja. Agen seharusnya milik Anda.</h1>

Codewhale adalah agen pemrograman bersumber terbuka untuk terminal Anda, ditulis dalam Rust. Ia bekerja dengan model apa pun, dapat membagi satu tugas ke beberapa agen, dan setiap barisnya bisa Anda baca.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Perintah ini memasang rilis terbaru di macOS atau Linux, dan tidak memerlukan akun. [Windows, npm, dan Cargo](#pemasangan) ada di bawah.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Situs web](https://codewhale.net) · [Dokumentasi](../README.md) · [Catatan perubahan](../../CHANGELOG.md) · [Berkontribusi](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Türkçe](README.tr.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Terjemahan ini adalah draf mesin yang belum ditinjau oleh penutur asli.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Codewhale di terminal setelah tugas demo. Ia melaporkan telah menambahkan flag --json ke export.py, ketiga tes lokal lulus, dan tinjauan yang didelegasikan tidak menemukan masalah. Daftar tugas di bawahnya menampilkan tiga langkah selesai, dan baris status menampilkan Full Access serta penyedia lokal website-demo.">

<sub>Build pengembangan 0.10.3, yang belum dirilis, dengan model uji berskrip, dalam Full Access di dalam ruang kerja sekali pakai. Pengeditan, tiga tes, dan tinjauan oleh sub-agen benar-benar dijalankan.</sub>

</div>

## Pemasangan

Perintah di atas menaruh `codewhale` yang sudah diverifikasi checksum dan alias pendeknya `codew` di `~/.local/bin`. Jika shell Anda lalu menampilkan `command not found`, ikuti catatan PATH yang dicetak pemasang ([detail](../INSTALL.md#put-it-on-your-path)). `codewhale update` memperbaruinya.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Biner siap pakai tersedia untuk Linux, macOS, dan Windows pada x64 dan arm64. Scoop, Nix, Homebrew di Linux, Android dan Termux (pratinjau), serta unduhan manual ada di [panduan pemasangan](../INSTALL.md), dan kontainer ada di [Docker](../DOCKER.md). Gunakan satu cara pemasangan per mesin; jika tidak, dua salinan akan ada di `PATH` dan yang ditemukan lebih dulu yang dipakai.

## Mulai cepat

1. Commit atau stash pekerjaan Anda, lalu jalankan `codewhale` dari root sebuah repositori git. Secara bawaan, ia menerapkan suntingan file di repositori tanpa bertanya, dan bertanya sebelum setiap perintah shell yang tidak hanya membaca.
2. Jalankan `/provider` (atau tekan `F3`), pilih penyedia, tempel kunci API-nya, dan pilih model. DeepSeek adalah penyedia bawaan. Pada peluncuran pertama tanpa kunci, Codewhale menanyakan penyedia saat Anda mengirim pesan pertama, dan menawarkan model lokal saat server Ollama lokal menjawab.
3. Ketik sebuah tugas:

```text
Fix the failing tests and explain what changed.
```

Perintah baca-saja langsung dijalankan. Untuk perintah shell lainnya, Codewhale bertanya dulu: `y` menjalankannya sekali, `a` mengizinkan perintah serupa selama sisa percakapan, dan `n` menolak. `/undo` membatalkan permintaan terakhir Anda beserta perubahan filenya, dan `/help` menampilkan daftar perintah dan tombol.

Untuk skrip dan CI, `codewhale exec` menjalankan satu tugas tanpa antarmuka. Baca [menjalankan tanpa antarmuka](../INSTALL.md#headless-scripts-ci) sebelum memberinya alat dengan `--auto`, karena `--auto` memakai Full Access kecuali Anda menambahkan sandbox.

## Apa yang dilakukannya

Beri Codewhale sebuah tugas, dan ia membaca repositori, mengedit file, dan menjalankan perintah sampai tugas selesai atau ia butuh masukan Anda. [Catatan rilis terbaru](https://github.com/codewhale-hq/Codewhale/releases/latest) mencantumkan perubahan terbaru, dan [changelog](../../CHANGELOG.md) memuat setiap versi.

### Model apa pun

Lebih dari 40 penyedia sudah terpasang, termasuk Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter, dan xAI. Endpoint apa pun yang kompatibel dengan OpenAI bisa dipakai, begitu juga model lokal melalui Ollama, vLLM, atau SGLang. Model bawaannya adalah `deepseek-flash` dari DeepSeek, yang kini dilayani DeepSeek V4.1 Flash ([detail](../PROVIDERS.md#deepseek-v41-flash)).

`/model` mengganti model di antara giliran, dan `/model auto` memilih satu model untuk setiap giliran (`/router` mengatur caranya). `/turn inspect` menampilkan model yang dipakai sebuah giliran, biayanya, dan berapa lama prosesnya. Lihat [Penyedia](../PROVIDERS.md).

### Tanda terima dan pembatalan

`/receipts`, atau `codewhale receipts --last` setelah Anda keluar, mencantumkan file yang diubah sebuah sesi, perintah yang dijalankannya, panggilan web dan MCP-nya, agen yang dimulainya, setiap persetujuan dan siapa yang memberikannya, serta apa yang gagal. `/undo` membatalkan permintaan terakhir Anda dan file yang diubahnya dalam satu langkah. `/restore` mengembalikan seluruh ruang kerja ke snapshot sebelumnya, dan karena ini menimpa suntingan sesudahnya, perintah ini memerlukan `/trust on` atau Full Access. Lihat [Tanda terima](../RECEIPTS.md).

### Beberapa agen pada satu tugas

`/goal` menetapkan sasaran yang berlanjut lintas giliran. Codewhale dapat membagi pekerjaan ke [sub-agen](../SUBAGENTS.md), menjalankan [Fleet](../FLEET.md) agen dengan peran dan model masing-masing, atau mengikuti [alur kerja](../WORKFLOW_AUTHORING.md) yang disimpan di repositori.

### Ekstensi

Hubungkan [server MCP](../MCP.md), tambahkan [skill](../SKILLS.md) dan [plugin](../PLUGINS.md), dan jalankan [hook](../HOOKS.md) pada peristiwa sesi dan alat. Mode kode (eksperimental, aktif secara bawaan) memungkinkan model menulis satu program `execute_tools` yang memanggil beberapa alat MCP dan plugin; setiap panggilan tetap mengikuti aturan persetujuan Anda dan menyimpan tanda terimanya. Plugin Computer Use yang dibundel dengan v0.10.2 (eksperimental, hanya macOS) dapat mengamati dan mengoperasikan aplikasi lain, dan tetap mati sampai Anda meninjau aksesnya dan mengaktifkannya ([panduan](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). Hadir di 0.10.3: plugin ini [dirilis terpisah](https://github.com/codewhale-hq/codewhale-cu-plugin).

### Cara menjalankannya

| Perintah | Fungsinya |
| --- | --- |
| `codewhale` | Aplikasi terminal interaktif |
| `codewhale exec "…"` | Satu tugas tanpa antarmuka, untuk skrip dan CI ([menjalankan tanpa antarmuka](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | Klien web lokal di `127.0.0.1` ([panduan](../WEB.md)) |
| `codewhale review --pr N` | Tinjauan bersifat saran untuk pull request GitHub; [pengaturan bot GitHub](../GITHUB_ACTION.md#set-up-your-own-bot) menjalankannya dengan kunci model Anda sendiri |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md), API HTTP lokal untuk thread, peristiwa, persetujuan, dan perubahan file setiap giliran |
| `codewhale serve --acp` | Agent Client Protocol melalui stdio, untuk editor seperti Zed |

Aplikasi terminal, `exec`, `web`, dan Runtime API memakai satu mesin yang sama ([arsitektur](../ARCHITECTURE.md)). [Ekstensi VS Code](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) yang dikelola komunitas terhubung dengannya dari bilah samping ([sumber](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Cara ia tetap aman

Dua sakelar menentukan seberapa banyak Codewhale boleh bertindak sendiri. Saat kolom input kosong, `Tab` mengganti mode: **Plan** membaca dan mencari tanpa mengubah apa pun, **Work** mengedit file dan menjalankan perintah, dan **Operate** punya akses yang sama dengan Work dan terus bekerja sampai tujuan terverifikasi atau ia membutuhkan Anda. `Shift+Tab` mengganti izin. **Ask**, bawaannya, menerapkan suntingan file di repositori Anda tanpa bertanya dan bertanya sebelum perintah shell yang tidak hanya membaca. **Auto-Review** membiarkan aturan dan model peninjau menyetujui panggilan rutin dan menolak yang berisiko tinggi; beralih ke Ask untuk menyetujuinya sendiri. **Full Access** tidak pernah bertanya dan mematikan sandbox Codewhale, meskipun aturan repositori dan kebijakan terkelola tetap berlaku. [Panduan v0.10.2](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md) berisi detailnya.

Hadir di 0.10.3: `Shift+Tab` menjadi satu tombol putar melalui Ask, Accept Edits, Plan, Auto-Review dan Full Access, `Tab` tidak lagi mengganti mode, dan Operate dihapus. Ask akan bertanya juga sebelum setiap suntingan file, dan bawaan baru, Accept Edits, akan menerapkan suntingan di dalam repositori git dan tetap bertanya sebelum perintah. [Panduan saat ini](../MODES.md) menjelaskannya.

Di Ask dan Auto-Review, perintah berjalan di sandbox sistem operasi jika sistemnya punya: Seatbelt di macOS, dan bubblewrap di Linux jika terpasang. Windows belum punya sandbox sistem operasi, dan Full Access mematikan sandbox. Di dalam sandbox, perintah shell dan sesi terminal tidak dapat membaca penyimpanan kredensial seperti `~/.ssh`, `~/.aws`, dan file `.env`. `/preview-request` menampilkan pratinjau permintaan berikutnya dengan data sensitif disamarkan, tanpa mengirimkannya.

Anda tidak memerlukan akun, dan kunci yang Anda tambahkan dengan `/provider` tetap di mesin Anda. Dengan kunci API sendiri, Anda membayar penyedia secara langsung, dan model lokal tidak memerlukan kunci. Footer dan `/cost` menampilkan biaya sebuah sesi jika harganya diketahui, dan harga yang tidak diketahui ditampilkan sebagai tidak diketahui, tidak pernah sebagai gratis.

Telemetri aktif secara bawaan. Ia mengirim hitungan gabungan (versi dan platform, lama dan hasil sesi, penyedia bawaan yang Anda pakai, hitungan fitur dan galat) dengan ID pemasangan acak yang berganti setiap 90 hari, dan tidak pernah mengirim percakapan, kode, prompt, nama model, nama file atau repositori, maupun kredensial. Perintah ini mematikannya:

```bash
codewhale config set telemetry false
```

Saat dimulai, Codewhale juga memeriksa GitHub untuk rilis baru; atur `CODEWHALE_NO_UPDATE_CHECK=1` untuk melewatinya. Selengkapnya di [urutan otorisasi](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md), dan [telemetri](../TELEMETRY.md).

## Ke mana arahnya

Codewhale sedang menjadi satu lingkungan yang mencakup terminal Anda, aplikasi desktop, browser, dan komputer cloud, semuanya dijalankan oleh satu mesin. Setiap bagian di bawah menyebutkan posisinya hari ini.

| Bagian | Status | Hari ini |
| --- | --- | --- |
| Aplikasi terminal | Dirilis | Instal di macOS, Linux, atau Windows dan jalankan `codewhale` di repositori mana pun. |
| Klien browser lokal | Dirilis | Jalankan `codewhale web` untuk membaca percakapan, mengirim tugas, dan menjawab persetujuan di browser Anda; klien ini menyertai terminal dan tidak memerlukan akun. |
| Banyak agen pada satu tugas | Dirilis | Sub-agen, Fleet, dan alur kerja memungkinkan beberapa agen berbagi satu tugas, masing-masing dengan perannya sendiri. |
| Aplikasi desktop | Dalam pengembangan, belum ada unduhan | Kami sedang membangun aplikasi desktop di atas mesin yang sama, dan belum ada unduhan publik. |
| Komputer cloud | Hanya undangan | Komputer cloud hanya untuk undangan, dan kami sedang membangunnya agar sebuah sesi dapat terus bekerja di mesin di cloud. |
| Satu sesi di semuanya | Sedang dibangun | Kami sedang membangun satu sesi yang ikut bersama Anda dari terminal ke desktop, browser, dan cloud, dan tetap milik Anda sepanjang jalan. |

[Peta jalan](../ROADMAP.md) menjelaskan cakupan setiap rilis dan di mana Anda bisa membantu.

## Pindah dari agen lain

Codewhale membaca `AGENTS.md` dengan sendirinya. File instruksi yang ditulis untuk alat lain, seperti `CLAUDE.md`, `.cursorrules`, atau `.github/copilot-instructions.md`, hanya dimuat jika Anda mengaktifkannya dengan `project_instruction_imports = ["claude"]` (atau `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) di konfigurasi; lihat [config.example.toml](../../config.example.toml). Codewhale memberi peringatan saat menemukan file seperti itu yang tidak dimuatnya.

`/import-claude` menyiapkan impor pengaturan Claude Code dan server MCP Anda untuk Anda tinjau. `/plugin install` menerima plugin Claude Code yang berisi skill, perintah, profil agen, dan server MCP, serta menolak plugin yang memerlukan hook atau server LSP ([detail](../CLAUDE_PLUGIN_COMPAT.md)).

## Dokumentasi

Panduan ini mengikuti cabang utama dan dapat menjelaskan perubahan yang belum ada di rilis terbaru.

| Mulai di sini | Referensi |
| --- | --- |
| [Panduan pengguna](../GUIDE.md) | [Konfigurasi](../CONFIGURATION.md) |
| [Pemasangan](../INSTALL.md) | [Arsitektur](../ARCHITECTURE.md) |
| [Penyedia dan model lokal](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Mode dan izin](../MODES.md) | [Menulis plugin](../PLUGIN_AUTHORING.md) |
| [Pintasan tombol](../KEYBINDINGS.md) | [Tanda terima](../RECEIPTS.md) |
| [Tinjauan pull request GitHub](../GITHUB_ACTION.md) | [Semua dokumentasi](../README.md) |

## Berkontribusi

Codewhale dibangun secara terbuka oleh orang-orang yang memakainya, dalam banyak bahasa.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Dinding kontributor Codewhale: satu avatar untuk setiap orang yang kode, commit bersama, pull request yang dilanjutkan, atau laporan yang diberi kredit ada di Codewhale, dengan jumlah total di atas."></a>

[Dinding kontributor](../CONTRIBUTORS.md) menghitung semua orang yang kode, commit bersama, pull request yang dilanjutkan, atau laporan yang diberi kredit ada di Codewhale, termasuk orang yang terlewat oleh daftar kontributor GitHub sendiri. `scripts/contributor-wall.py` membuat ulang gambar dan daftar itu dari riwayat git dan GitHub.

Semua masukan diterima: bug, ide, atau sesuatu yang tampak atau terasa salah. [Buka issue](https://github.com/codewhale-hq/Codewhale/issues/new/choose) untuk semua itu; issue juga tempat terbaik untuk memulai fitur baru. Pull request diterima untuk perbaikan, terjemahan, penyedia, dan pekerjaan yang sudah direncanakan. Pertanyaan bisa diajukan di [Discussions](https://github.com/codewhale-hq/Codewhale/discussions). [Panduan kontribusi](../../CONTRIBUTING.md) menunjukkan ke mana setiap jenis bantuan pergi, bagaimana pekerjaan Anda masuk dengan nama Anda, dan [di mana semuanya berada](../../CONTRIBUTING.md#project-structure).

Mengobrollah di [Discord](https://discord.gg/37gfS3ksug), atau tambahkan pengelola, Hunter, di WeChat (`hunterbown`) dan minta bergabung dengan grup Whale Brothers. Semua orang dipersilakan.

## Sejarah dan lisensi

Codewhale bermula sebagai `deepseek-tui` dan masih membaca konfigurasi dan sesi proyek itu. Ia dikelola secara independen dan tidak berafiliasi dengan DeepSeek atau penyedia model lain mana pun.

[MIT](../../LICENSE). Kode yang diadaptasi dari proyek sumber terbuka lain tercantum dalam pemberitahuan pihak ketiga untuk [sumber yang disertakan](../../THIRD_PARTY_NOTICES.md) dan [desain yang diadaptasi](../THIRD_PARTY_NOTICES.md).
