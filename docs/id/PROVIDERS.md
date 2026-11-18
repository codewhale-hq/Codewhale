# Registri Penyedia (Provider Registry)

> Dokumen referensi lengkap dalam bahasa Inggris: [PROVIDERS.md](../PROVIDERS.md). Aturan resolusi ID model, alias per penyedia, batas konteks/output, dan pembaruan `registry.json` bertanda tangan dikelola di satu dokumen pemilik: [MODELS_AND_PROVIDERS.md](../MODELS_AND_PROVIDERS.md).

Registri ini menjelaskan perilaku penyedia (provider) yang terhubung ke dalam basis kode Codewhale saat ini. Registri ini sengaja dibuat konservatif: entri yang dirilis terbatas pada ID penyedia, kunci konfigurasi, jalur autentikasi, URL dasar, resolusi model, dan metadata kapabilitas yang sudah dikelola oleh sistem.

DeepSeek tetap menjadi penyedia bawaan (default), tetapi setiap entri dalam `ProviderKind::ALL` dan `PROVIDER_REGISTRY` adalah rute penyedia yang dapat dipilih sebagai warga kelas satu. Rute ter-host, endpoint generik kompatibel-OpenAI, rute OpenAI Codex/ChatGPT, Anthropic native, dan runtime lokal semuanya menjalankan harness terminal yang sama terhadap penyedia/model/URL dasar terpilih.

---

## Pemilihan Penyedia (Provider Selection)

ID penyedia kanonik yang didukung meliputi:

`deepseek`, `deepseek-anthropic`, `nvidia-nim`, `openai`, `atlascloud`, `wanjie-ark`, `volcengine`, `openrouter`, `xiaomi-mimo`, `novita`, `fireworks`, `siliconflow`, `arcee`, `siliconflow-CN`, `moonshot`, `sglang`, `vllm`, `ollama`, `huggingface`, `together`, `qianfan`, `openai-codex`, `anthropic`, `openmodel`, `zai`, `stepfun`, `minimax`, `deepinfra`, `sakana`, `longcat`, `opencode-go`, `meta`, `telecomjs`, dan `xai`.

Gunakan salah satu cara berikut untuk memilih penyedia:

- CLI: `codewhale --provider <id>`
- TUI: `/provider <id>` atau melalui pemilih penyedia (provider picker)
- Variabel Lingkungan: `CODEWHALE_PROVIDER=<id>` (atau alias lama `DEEPSEEK_PROVIDER=<id>`)
- Konfigurasi (`config.toml`): `provider = "<id>"`

---

## Ringkasan Rute Utama

1. **DeepSeek (`deepseek`)**:
   - URL Dasar: `https://api.deepseek.com`
   - Model bawaan: `deepseek-flash` (dengan `deepseek-v4-pro` untuk tugas berat)
   - Autentikasi: `DEEPSEEK_API_KEY` (Bearer auth)

2. **OpenAI (`openai`)**:
   - URL Dasar: `https://api.openai.com/v1`
   - Model bawaan: `gpt-4o`, `gpt-4o-mini`, `o1`, `o3-mini`
   - Autentikasi: `OPENAI_API_KEY`

3. **Anthropic (`anthropic`)**:
   - URL Dasar: `https://api.anthropic.com`
   - Model bawaan: `claude-3-5-sonnet-latest`, `claude-3-5-haiku-latest`
   - Autentikasi: `ANTHROPIC_API_KEY` (`x-api-key`)

4. **Runtime Lokal (`vllm`, `ollama`, `sglang`)**:
   - Ollama bawaan: `http://127.0.0.1:11434/v1`
   - vLLM / SGLang bawaan: `http://127.0.0.1:8000/v1`
   - Tidak memerlukan API key untuk eksekusi lokal.
