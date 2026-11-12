# Integrasi MCP (Server Alat Eksternal)

Codewhale dapat memuat alat tambahan melalui **MCP (Model Context Protocol)**. Server MCP dapat berupa proses stdio lokal yang dijalankan oleh TUI, atau server jarak jauh berbasis URL yang menggunakan Streamable HTTP dengan fallback SSE.

---

## Konfigurasi Server MCP

Konfigurasi server MCP disimpan di dalam `mcp.json` (baik di `$CODEWHALE_HOME/mcp.json` untuk global atau `.codewhale/mcp.json` untuk proyek).

File proyek `.codewhale/mcp.json` hanya dimuat setelah workspace dipercaya di konfigurasi milik pengguna **dan** Anda menyetujui isi persis file tersebut. Entri stdio adalah baris perintah, dan server yang bukan `lazy` berjalan saat sesi dimulai, sehingga file proyek adalah konfigurasi yang dapat dieksekusi, seperti hook proyek. Gunakan `/mcp review` untuk melihat file beserta digest-nya, lalu `/mcp approve <digest>` untuk memuat isi tersebut. Persetujuan disimpan di konfigurasi milik pengguna pada entri `projects` workspace (`mcp_sha256`), bukan di repositori, dan `.codewhale` atau `mcp.json` berupa symlink ditolak. Setiap perubahan file berikutnya, misalnya karena pull atau pindah branch, membuat tidak ada servernya yang dimuat sampai Anda meninjau dan menyetujui lagi. `/mcp revoke` mencabut persetujuan. `/mcp disable`, `/mcp enable` dan `/mcp remove` pada server proyek mengubah file dan mempertahankan persetujuannya. Tinjau juga skrip yang dijalankan perintah server: persetujuan hanya mencakup file itu sendiri. `codewhale doctor` dan `/mcp validate` melaporkan file proyek yang menunggu persetujuan.

Contoh konfigurasi `mcp.json`:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
    },
    "git": {
      "command": "uvx",
      "args": ["mcp-server-git", "--repository", "."]
    }
  }
}
```

---

## Perintah CLI & TUI untuk MCP

- `/mcp` — Kelola server dan alat MCP dari dalam TUI (`/mcp review`, `/mcp approve <digest>`, `/mcp revoke` untuk file proyek).
- `codewhale mcp init` — Inisialisasi templat konfigurasi `mcp.json`.
- `codewhale doctor` — Periksa kesehatan dan kesiapan server MCP yang terkonfigurasi.
