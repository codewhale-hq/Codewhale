<!-- source: README.md sha256:7e0e7ebc680b -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Terminaliniz için Rust ile yazılmış açık kaynaklı kodlama ajanı. Her model, çoklu ajan filoları, işletim sistemi korumalı alanı ve doğrulanabilir kayıtlar.**

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Web sitesi](https://codewhale.net) · [Belgeler](../README.md) · [Değişiklik günlüğü](../../CHANGELOG.md) · [Katkıda bulunma](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-latest.png" width="760" alt="Yerel Ollama (qwen3:4b) üzerinde gerçek bir görevden sonra terminalde Codewhale v0.10.4: ls.py dosyasını düzenledi, python3 -m unittest -v test_ls.py komutunu çalıştırmadan önce onay bekledi, 2 başarılı testi çalıştırdı ve sonucu bildirdi.">

<sub>Accept Edits modunda yerel Ollama (qwen3:4b) çalıştıran yayımlanmış Codewhale v0.10.4 (48669ad108fa) üzerinde gerçek oturum.</sub>

</div>

## Kurulum

Yukarıdaki kurulum betiği, sağlama toplamı doğrulanmış `codewhale` ikilisini ve kısa takma adı `codew` komutunu macOS ve Linux'ta `~/.local/bin` dizinine yerleştirir. Kabuğunuz `command not found` hatası verirse `~/.local/bin` dizinini `PATH` değişkenine ekleyin ([ayrıntılar](../INSTALL.md#put-it-on-your-path)). Yerinde güncellemek için `codewhale update` komutunu çalıştırın.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Hazır ikili dosyalar Linux, macOS ve Windows (x64 ve arm64) için yayımlanır. Scoop, Nix, Linux üzerinde Homebrew, Android/Termux (önizleme) ve manuel indirmeler [docs/INSTALL.md](../INSTALL.md) içinde; konteyner imajları ise [docs/DOCKER.md](../DOCKER.md) içinde yer alır. İki ikilinin `PATH` üzerinde birbirini gölgelememesi için her makinede tek bir kurulum yöntemi kullanın.

## Hızlı başlangıç

1. Çalışmanızı commit veya stash yapın, ardından bir git deposunun kök dizininde `codewhale` komutunu çalıştırın.
2. Bir sağlayıcı seçmek, API anahtarı yapıştırmak ve model seçmek için `/provider` komutunu çalıştırın (veya `F3` tuşuna basın). DeepSeek varsayılandır; anahtar yapılandırılmadıysa ve yerel bir Ollama sunucusu yanıt veriyorsa, Codewhale ilk mesajınızda onun modellerini önerir.
3. Bir görev verin:

```text
Fix the failing tests and explain what changed.
```

Salt okunur komutlar hemen çalışır. Değişiklik yapan kabuk komutları onay ister: `y` bir kez çalıştırır, `a` oturumun geri kalanında eşleşen komutlara izin verir, `n` reddeder. Son turu ve dosya düzenlemelerini geri almak için `/undo`, komutlar ve kısayollar için `/help` yazın.

## Yetenekler

- **50+ yerleşik sağlayıcı ve yerel modeller:** Anthropic, OpenAI, DeepSeek (varsayılan olarak `deepseek-flash` / V4.1 Flash; [ayrıntılar](../PROVIDERS.md#deepseek-v41-flash)), Google, Mistral, Moonshot, OpenRouter, xAI, herhangi bir OpenAI uyumlu uç nokta ve Ollama, vLLM veya SGLang üzerinden yerel çalışma zamanları. Oturum ortasında `/model` ile model değiştirin, `/model auto` (`/router`) ile tur bazlı yönlendirin ve `/turn inspect` ile gecikme ve maliyeti inceleyin. Bkz. [Sağlayıcılar](../PROVIDERS.md).
- **Kayıtlar ve geri alma:** `/receipts` (veya çıktıktan sonra `codewhale receipts --last`), değişen dosyaları, komutları, web/MCP çağrılarını, alt ajanları, onayları ve hataları kaydeder. `/undo` son turu tek adımda geri alır; `/restore` çalışma alanını önceki bir anlık görüntüye döndürür (`/trust on` veya Full Access gerektirir). Bkz. [Kayıtlar](../RECEIPTS.md).
- **Alt ajanlar, filolar ve iş akışları:** `/goal` ile çok turlu hedefleri sürdürün, işleri [alt ajanlara](../SUBAGENTS.md) devredin, rol ve modele özel [filoları (Fleets)](../FLEET.md) koordine edin veya depoda saklanan [iş akışlarını](../WORKFLOW_AUTHORING.md) çalıştırın.
- **MCP, beceriler, kancalar ve eklentiler:** [MCP sunucuları](../MCP.md), [beceriler](../SKILLS.md), [eklentiler](../PLUGINS.md) ve yaşam döngüsü [kancaları](../HOOKS.md) bağlayın. Deneysel kod modu (`execute_tools`), birden fazla MCP/eklenti çağrısını normal onay kurallarınız altında topluca çalıştırır. İsteğe bağlı macOS [Computer Use](https://github.com/codewhale-hq/codewhale-cu-plugin) eklentisi, siz açıkça inceleyip etkinleştirene kadar kapalı kalır.
- **Doğrudan uyumluluk:** `AGENTS.md` dosyasını otomatik okur. [config.example.toml](../../config.example.toml) içindeki `project_instruction_imports` ile `CLAUDE.md`, `.cursorrules` veya `.github/copilot-instructions.md` dosyalarını etkinleştirin. `/import-claude` ile Claude Code ayarlarını ve MCP sunucularını içe aktarın ya da `/plugin install` ile Claude Code eklentilerini (beceriler, komutlar, ajan profilleri, MCP) ve DeepSeek Harness (DSH) eklentilerini kurun ([ayrıntılar](../CLAUDE_PLUGIN_COMPAT.md)).

## Giriş noktaları

| Komut | Amaç |
| --- | --- |
| `codewhale` | Etkileşimli terminal uygulaması |
| `codewhale exec "…"` | Betikler ve CI için arayüzsüz tek görev çalıştırıcısı ([kılavuz](../INSTALL.md#headless-scripts-ci); `--auto`, korumalı alan yoksa Full Access kullanır) |
| `codewhale web` | `127.0.0.1` üzerinde yerel web istemcisi ([kılavuz](../WEB.md)) |
| `codewhale review --pr N` | Tavsiye niteliğinde GitHub PR incelemesi; [GitHub bot kurulumu](../GITHUB_ACTION.md#set-up-your-own-bot) kendi anahtarınızla çalışır |
| `codewhale app-server --http` | İş parçacıkları, olaylar, onaylar ve tur bazlı farklar için yerel HTTP [Runtime API](../RUNTIME_API.md) |
| `codewhale serve --acp` | Zed gibi editörler için stdio üzerinden Agent Client Protocol |

Tüm giriş noktaları tek bir Rust motorunu paylaşır ([mimari](../ARCHITECTURE.md)). Topluluk tarafından sürdürülen [VS Code eklentisi](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) ([kaynak](https://github.com/HengQuWorld/CodeWhale-VSCode)) aynı motora bağlanır. Güncel sürüm ve masaüstü/bulut durumu için [son sürüm notlarına](https://github.com/codewhale-hq/Codewhale/releases/latest) ve [yol haritasına](../ROADMAP.md) bakın.

## İzinler, korumalı alan ve gizlilik

`Shift+Tab` izin modları arasında geçiş yapar: **Ask → Accept Edits → Plan → Auto-Review → Full Access** (`Tab` yalnızca tamamlama yapar). **Accept Edits** (varsayılan) git deposu içindeki düzenlemeleri uygular ve komutlardan önce sorar. **Ask** her düzenleme ve komutta sorar. **Plan** salt okunurdur. **Auto-Review** rutin çağrıları onaylamak ve yüksek riskli olanları engellemek için kurallar ile bir inceleme modeli kullanır. **Full Access** onay istemlerini atlar ve işletim sistemi korumalı alanını devre dışı bırakır (depo kuralları ve yönetilen politikalar yine de uygulanır). Bkz. [Modlar](../MODES.md).

Ask, Accept Edits ve Auto-Review modlarında komutlar mümkün olan yerlerde bir işletim sistemi korumalı alanı içinde çalışır: macOS'ta Seatbelt ve Linux'ta bubblewrap (Windows'ta henüz işletim sistemi korumalı alanı yoktur). Korumalı alandaki komutlar ve terminal oturumları `~/.ssh`, `~/.aws` veya `.env` gibi kimlik bilgisi yollarını okuyamaz. Göndermeden önce gizlenmiş bir giden isteği incelemek için `/preview-request` çalıştırın.

`/provider` ile eklenen API anahtarları makinenizde kalır ve yerel modeller anahtar gerektirmez. `/cost` ve alt bilgi, fiyatlandırma bilindiğinde oturum harcamasını izler (bilinmeyen fiyat ücretsiz değil, bilinmeyen olarak gösterilir).

Toplu telemetri varsayılan olarak açıktır (sürüm, işletim sistemi, oturum süresi/sonucu, yerleşik sağlayıcı kimlikleri, yerleşik genel katalog model kimlikleri (özel, gizli veya yerel modeller `other` olarak kaydedilir), toplam belirteç sayıları, yerleşik araç çağrı/hata sayıları ve özellik/hata sayaçları; 90 günde bir değişen rastgele kurulum kimliği; asla kod, istem, sohbet, özel/gizli model adları, dosya/depo/dal adları, kimlik bilgileri veya IP adresleri gönderilmez). Kapatmak için:

```bash
codewhale config set telemetry false
```

Codewhale başlangıçta GitHub'da yeni sürüm olup olmadığını da denetler (atlamak için `CODEWHALE_NO_UPDATE_CHECK=1`) ve genel `models.dev` model kataloğunu her 24 saatte bir yeniler (atlamak için `CODEWHALE_DISABLE_MODELS_DEV_FETCH=1` veya `[catalog] auto_refresh = false`; CI ortamlarında ve `DO_NOT_TRACK` etkinken otomatik olarak kapatılır). Ayrıntılar: [yetkilendirme sırası](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md) ve [telemetri](../TELEMETRY.md).

## Belgeler

Kılavuzlar `main` dalını izler ve henüz son sürümde yer almayan değişiklikleri anlatabilir.

| Buradan başlayın | Başvuru |
| --- | --- |
| [Kullanıcı kılavuzu](../GUIDE.md) | [Yapılandırma](../CONFIGURATION.md) |
| [Kurulum](../INSTALL.md) | [Mimari](../ARCHITECTURE.md) |
| [Sağlayıcılar ve yerel modeller](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modlar ve izinler](../MODES.md) | [Eklenti geliştirme](../PLUGIN_AUTHORING.md) |
| [Kısayollar](../KEYBINDINGS.md) | [Kayıtlar](../RECEIPTS.md) |
| [GitHub PR incelemesi](../GITHUB_ACTION.md) | [Tüm belgeler](../README.md) |

## Katkıda bulunma ve lisans

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale katkıda bulunanlar duvarı"></a>

[Katkıda bulunanlar duvarı](../CONTRIBUTORS.md) (`scripts/contributor-wall.py`) kodu, ortak yazarlı commit'leri, taşınan PR'ları ve kabul edilen raporları izler. Hata raporları ve özellik önerileri için [Issues](https://github.com/codewhale-hq/Codewhale/issues/new/choose), sorular için [Discussions](https://github.com/codewhale-hq/Codewhale/discussions), PR yönergeleri ve proje düzeni için [CONTRIBUTING.md](../../CONTRIBUTING.md) ([proje yapısı](../../CONTRIBUTING.md#project-structure)) sayfalarını kullanın. [Discord](https://discord.gg/37gfS3ksug) sunucumuza katılın veya Whale Brothers grubu için WeChat'te `hunterbown` hesabını ekleyin.

Codewhale `deepseek-tui` olarak başladı ve onun yapılandırması ile oturumlarıyla geriye dönük uyumluluğunu korur. Bağımsız olarak sürdürülür ve DeepSeek veya başka bir model sağlayıcısıyla bağlantılı değildir.

[MIT](../../LICENSE). Üçüncü taraf bildirimleri: [dahil edilen kaynaklar](../../THIRD_PARTY_NOTICES.md) ve [uyarlanan tasarımlar](../THIRD_PARTY_NOTICES.md).
