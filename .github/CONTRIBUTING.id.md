# Berkontribusi pada Codewhale

Codewhale berkembang berkat orang yang menggunakannya, melaporkan masalah,
menerjemahkan dokumentasi, menambahkan penyedia model, dan mengirim perbaikan.
Anda dapat berkontribusi tanpa menulis kode. Semua masukan diterima.

[Roadmap](../docs/ROADMAP.md) menjelaskan tujuan proyek dan pekerjaan yang masih
terbuka. [Panduan utama](../CONTRIBUTING.md) memuat prosedur pengembangan,
pengujian, dan peninjauan yang lengkap dan terbaru.

## Pilih tempat yang sesuai

| Yang ingin Anda sampaikan | Tempatnya |
| --- | --- |
| Bug, crash, atau perilaku yang tidak diharapkan | [Laporan bug](https://github.com/codewhale-hq/CodeWhale/issues/new?template=bug_report.yml) |
| Ide atau permintaan fitur | [Permintaan fitur](https://github.com/codewhale-hq/CodeWhale/issues/new?template=feature_request.yml) |
| Masukan tentang tampilan, tulisan, atau pengalaman penggunaan | [Formulir masukan](https://github.com/codewhale-hq/CodeWhale/issues/new?template=feedback.yml) |
| Pertanyaan | [Tanya jawab](https://github.com/codewhale-hq/CodeWhale/discussions/categories/q-a) atau [Discord](https://discord.gg/37gfS3ksug) |
| Masalah keamanan | [Laporan keamanan privat](https://github.com/codewhale-hq/CodeWhale/security/advisories/new), bukan issue publik |
| Perbaikan untuk issue yang sudah ada | Pull request yang menautkan issue tersebut |
| Terjemahan, penyedia model, atau perbaikan dokumentasi | Pull request; tidak perlu membuka issue terlebih dahulu |

Jika ragu, buka issue dan jelaskan apa yang Anda coba lakukan, apa yang terjadi,
dan apa yang Anda harapkan. Pengelola akan membantu mengarahkannya.

## Mulai fitur baru dengan issue

Untuk fitur baru, buka issue sebelum mulai menulis kode. Jelaskan kebutuhan dan
hambatannya; rancangan solusi boleh disertakan, tetapi tidak wajib. Jika ingin
mengerjakannya sendiri, centang pilihan untuk membuka pull request pada formulir.

Pengelola dapat membahas cakupan dan perilaku fitur sebelum Anda menghabiskan
waktu untuk implementasi. Pull request fitur tanpa issue tetap dibaca, tetapi
pembahasannya biasanya membutuhkan waktu lebih lama.

## Pekerjaan yang dapat langsung Anda kirim

Anda dapat langsung mengirim pull request untuk terjemahan, penyedia dan model,
perbaikan dokumentasi, atau perbaikan bug. Jika bug belum memiliki issue,
jelaskan cara memunculkannya, hasil yang terjadi, dan hasil yang diharapkan di
pull request.

Untuk pekerjaan yang sudah direncanakan, pilih issue di
[milestone](../docs/ROADMAP.md#releases) atau yang berlabel
[help wanted](https://github.com/codewhale-hq/CodeWhale/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22).
Beri komentar bahwa Anda ingin mengerjakannya agar pekerjaan tidak tumpang tindih.
[Lihat proyek terbuka](../CONTRIBUTING.md#standing-projects) untuk tautan ke
panduan terjemahan dan penyedia model.

## Siapkan perubahan Anda

Ikuti [petunjuk persiapan pengembangan](../CONTRIBUTING.md#getting-started)
untuk persyaratan Rust, perintah build, dan cara menjalankan versi sumber.
Versi di `main` dapat lebih baru daripada unduhan rilis; sertakan keluaran
`codewhale --version` ketika melaporkan masalah.

Jalankan pemeriksaan yang sesuai dengan bagian yang Anda ubah, mengikuti
[panduan pengujian lokal](../CONTRIBUTING.md#fast-local-loop). Sebelum mengirim
pull request, baca perubahan Anda sendiri dan jelaskan apa yang berubah serta
bagaimana Anda memeriksanya. Jangan sertakan kunci API atau data pribadi.

## Peninjauan dan kredit

Pull request Anda digabungkan sebagai kontribusi Anda sendiri. Pengelola
membantu menyelesaikan konflik akibat perubahan di `main`; pemeriksaan yang
merah karena masalah lain bukan tanggung jawab Anda. Jika perubahan belum siap,
pengelola menjelaskan alasannya. Kontribusi dicatat di changelog dan
[daftar kontributor](../docs/CONTRIBUTORS.md).
