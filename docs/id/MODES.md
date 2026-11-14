# Mode dan Postur Izin (Modes and Permission Postures)

> Terjemahan ini belum mencakup pembaruan Auto-Review, asal giliran (*run origin*), dan kehadiran pemilik terkini; rujuk [docs/MODES.md](../MODES.md) untuk spesifikasi terbaru.

Codewhale memiliki tiga konsep yang saling berhubungan:

- **Mode TUI**: jenis interaksi yang terlihat (Plan / Work / Operate).
- **Postur Izin**: seberapa ketat antarmuka meminta persetujuan sebelum mengeksekusi alat.
- **Lapisan Alur Kerja (Workflow Overlay)**: orkestrasi berjangka panjang opsional yang dapat berjalan di atas mode TUI mana pun ketika tugas membutuhkan banyak pekerja terkoordinasi.

---

## Mode TUI

Tekan `Tab` saat composer dalam keadaan diam untuk beralih antar mode: **Plan → Act → Operate → Plan**.
Tekan `Shift+Tab` untuk beralih postur izin (**Ask → Auto-Review → Full Access**).
Tekan `Ctrl+T` untuk beralih tingkat penalaran (reasoning effort).
Jalankan `/mode` untuk membuka pemilih mode, atau beralih secara langsung dengan `/mode act`, `/mode plan`, atau `/mode operate`.

### 1. Plan (Perencanaan)
- Mengutamakan perancangan dan analisis sebelum eksekusi.
- Alat investigasi baca-saja (*read-only*) tetap tersedia; eksekusi shell dan pengeditan berkas dinonaktifkan.

### 2. Act (Eksekusi / Agen)
- Penggunaan alat multi-langkah.
- Perintah `Bash` dan pengeditan berkas dilindungi oleh gerbang persetujuan secara bawaan.

### 3. Operate (Armada / Orkestrasi)
- Mode konduktor multi-tugas.
- Menugaskan pekerja di latar belakang (*background workers*) sebagai cara utama untuk menangani tugas-tugas besar atau independen secara paralel.

---

## Ketersediaan Alat Berdasarkan Mode

| Keluarga Alat | Plan | Act | Operate |
|:---|:---:|:---:|:---:|
| Alat baca berkas, pencarian, dan diagnostik | Ya | Ya | Ya |
| Alat penulisan berkas dan penambalan | Tidak | Ya | Ya |
| Perintah `Bash` | Tidak | Memerlukan persetujuan | Sama dengan Act |
| Akses di luar root ruang kerja | Jalur terpercaya saja | Jalur terpercaya / mode trust | Sama dengan Act |
