# Konfigurasi Codewhale

Codewhale membaca konfigurasi dari berkas TOML ditambah variabel lingkungan (environment variables). Saat startup proses, Codewhale juga dapat membaca kredensial penyedia dari berkas `.env` lokal di ruang kerja. Gunakan contoh templat `.env.example` yang tersedia; salin ke `.env`, lalu tambahkan hanya nilai kredensial.

---

## Konstitusi, Instruksi Proyek, dan Wewenang Repositori

Codewhale memiliki beberapa lapisan instruksi yang sengaja dipisahkan agar konstitusi pribadi, kebijakan repositori, instruksi proyek, dan kontrol keamanan runtime tidak saling bertabrakan:

1. **Konstitusi Global Bawaan (Bundled Constitution)**:
   - Hukum dasar yang terkompilasi di dalam biner. Menjadi batas dasar bawaan untuk setiap sesi.

2. **Konstitusi Global Pengguna (User-Global Constitution)**:
   - Dikelola melalui perintah `/constitution` atau `/setup`. Ditinggalkan dalam format terstruktur pada `$CODEWHALE_HOME/constitution.json` (bawaan `~/.codewhale/constitution.json`).

3. **Konstitusi Lokal Repositori (Repo-Local Constitution)**:
   - Kebijakan proyek opsional pada `.codewhale/constitution.json`.

4. **Instruksi Proyek (`AGENTS.md`)**:
   - Berkas instruksi proyek lintas-agen. Berkas ini adalah dokumen kanonik untuk "bagaimana agen bekerja di repositori ini". Jalankan `/init` untuk membuatnya secara otomatis. `CLAUDE.md` dibaca sebagai fallback kompatibilitas.

5. **Memori dan Serah Terima (Memory & Handoffs)**:
   - Riwayat dan konteks yang dipanggil kembali. Berfungsi membantu, namun memiliki tingkat wewenang lebih rendah dari konstitusi dan instruksi proyek.

---

## Lokasi Berkas Konfigurasi

- Berkas Konfigurasi Utama: `~/.codewhale/config.toml`
- Berkas Konstitusi Pengguna: `~/.codewhale/constitution.json`
- Berkas Konfigurasi MCP: `~/.codewhale/mcp.json`
- Berkas Sesi: `~/.codewhale/sessions/`

## Kebijakan Skill Bawaan

`[skills.bundled]` memisahkan pemasangan dari penemuan skill. `install` menerima `"auto"` (bawaan), `"all"`, atau `"none"`; `selected`, `disabled`, dan `explicit_only` mengatur nama skill bawaan tanpa menghapus berkas atau menimpa salinan yang diedit pengguna. Lihat [panduan lengkap](../SKILLS.md#bundled-skills) dan jalankan `codewhale doctor` atau `/skills inspect` untuk status yang berlaku.
