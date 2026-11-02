<!-- source: README.md sha256:0430d94f3d47 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/wordmark-inverted.svg">
  <img src="../../brand/wordmark.svg" alt="Codewhale" width="320">
</picture>

**Terminaliniz için her modelle çalışan, açık kaynaklı bir kodlama ajanı.**

Codewhale'e bir görev verin; görev bitene ya da sizden girdi gerekene kadar depoyu
okur, dosyaları düzenler ve komutları çalıştırır. 40'tan fazla yerleşik sağlayıcıyla
(varsayılan olarak DeepSeek V4.1 Flash) ve kendi makinenizde çalışan modellerle
çalışır.
`/receipts`, değiştirdiği dosyaları, çalıştırdığı komutları ve yaptığı web ve MCP
çağrılarını listeler.

[![CI](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/CodeWhale/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/codewhale-cli?label=crates.io)](https://crates.io/crates/codewhale-cli)
[![npm](https://img.shields.io/npm/v/codewhale?label=npm)](https://www.npmjs.com/package/codewhale)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Web sitesi](https://codewhale.net) · [Belgeler](../README.md) · [Değişiklik günlüğü](../../CHANGELOG.md) · [Katkıda bulunma](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<img src="../../web/public/codewhale-tui-0102-demo.png" alt="Bir Codewhale terminal oturumu" width="760">

<sub>Betikli bir test modeliyle, atılabilir bir çalışma alanında Full Access ile çalışan bir 0.10.2 geliştirme derlemesi. Düzenleme, üç test ve bir alt ajan incelemesi gerçekten çalıştı.</sub>

</div>

<p align="center">
  <a href="https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67" rel="sponsored">
    <img src="../../web/public/codewhale-x-orcarouter.webp" alt="Codewhale × OrcaRouter — %30 daha fazla DeepSeek v4.1 Flash kullanımı" width="760">
  </a>
</p>

[Codewhale × OrcaRouter teklifini görüntüleyin](https://www.orcarouter.ai/redeem/1a6250545fdc4e0aaccb0a4470159e67). Codewhale, OrcaRouter’dan yönlendirme komisyonu alır.

## Kurulum

macOS ve Linux:

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Kurulum betiği, sağlama toplamı doğrulanmış `codewhale` ikili dosyasını ve kısa
takma adı olan `codew` komutunu `~/.local/bin` dizinine koyar. Ardından kabuğunuz
`command not found` derse, betiğin yazdırdığı PATH satırını çalıştırın
([ayrıntılar](../INSTALL.md#put-it-on-your-path)). `codewhale update` kurulumu
yükseltir. Bu komutlar en son sürümü kurar. Bu sayfa `main` dalını izler; bu yüzden
bir sürüm numarasıyla işaretlenen özellik o sürümle gelir. Her sürümün neler
eklediğini [değişiklik günlüğü](../../CHANGELOG.md) listeler.

<details>
<summary><b>Windows, npm, Cargo ve diğer kurulum yolları</b></summary>

```bash
winget install HunterBown.CodeWhale  # Windows x64 (or Scoop, or the installer from GitHub Releases)
npm install -g codewhale            # wraps the same release binaries
cargo install codewhale-cli --locked  # crates.io; can trail GitHub Releases
```

Hazır derlenmiş ikili dosyalar x64 ve arm64 üzerinde Linux, macOS ve Windows'u
kapsar. Nix, Linux'ta Homebrew, Android/Termux (önizleme) ve elle indirme
[kurulum kılavuzunda](../INSTALL.md), konteynerler ise [Docker](../DOCKER.md)
sayfasında anlatılır. Her makinede tek bir kurulum yöntemi kullanın; aksi halde
`PATH` üzerinde iki kopya olur ve hangisi önce gelirse o çalışır.

</details>

## Hızlı başlangıç

1. Çalışmanızı commit edin veya stash'leyin, ardından bir git deposunun kök
   dizininde `codewhale` çalıştırın. Varsayılan olarak orada dosya düzenlemelerini
   sormadan uygular ve size diff'i gösterir.
2. `/provider` çalıştırın (veya `F3` tuşuna basın), bir sağlayıcı seçin, onun API
   anahtarını yapıştırın ve bir model seçin. Varsayılan sağlayıcı DeepSeek'tir.
   Anahtarınız yoksa bunun yerine yerel bir çalışma ortamı seçin. Hiçbir sağlayıcı
   yapılandırılmamışsa ve Ollama zaten bir sohbet modeli sunuyorsa Codewhale onu
   kendiliğinden kullanır; 0.10.2'den itibaren ilk mesajınızı gönderdiğinizde onu
   kullanmayı önerir.
3. Bir görev yazın:

```text
Fix the failing tests and explain what changed.
```

Salt okunur komutlar hemen çalışır. Diğer her kabuk komutu için Codewhale önce
sorar: `y` komutu bir kez çalıştırır, `a` konuşmanın geri kalanında benzer
komutlara izin verir, `n` reddeder. `/undo` son isteğinizi ve onun dosya
değişikliklerini geri alır. `/help`
komutları ve tuşları listeler.

Betikler ve CI için `codewhale exec`, tek bir görevi arayüz olmadan çalıştırır.
Varsayılan olarak `--auto`, Full Access kullanır: araçlar sormadan çalışır,
kabuk komutları için Codewhale korumalı alanı yoktur ve güven modu, dosya
araçlarının çalışma alanı dışına yazmasına izin verir. Kabuk korumalı alanını
ve dosya araçlarının çalışma alanı kısıtlamalarını güven modu kapalı olarak
korumak için `--sandbox workspace-write` ekleyin:

```bash
codewhale exec --auto --sandbox workspace-write "fix the failing tests and explain what changed"
```

## Modlar ve izinler

Yeni bir kurulum Work modunda ve Ask izniyle başlar.

| | Nasıl değiştirilir | Seçenekler |
| --- | --- | --- |
| Mod | `Tab` (giriş alanı boşken) veya `/mode` | **Plan** okur ve arar ama hiçbir şeyi değiştirmez. **Work** dosyaları düzenler ve komutları çalıştırır. **Operate** Work ile aynı erişime sahiptir, isteğinizi bir hedef olarak ele alır ve hedef doğrulanana ya da sizden girdi gerekene kadar devam eder. |
| İzin | `Shift+Tab` | **Ask** deponuzdaki dosya düzenlemelerini sormadan uygular, salt okunur olmayan kabuk komutlarından önce sorar. **Auto-Review** rutin çağrıların kurallar ve bir inceleyici model tarafından onaylanmasına izin verir, yüksek riskli olanları reddeder; bunları kendiniz onaylamak için Ask iznine geçin. **Full Access** hiç sormaz ve Codewhale'in korumalı alanını kapatır; depo kuralları ve yönetilen politika yine de geçerlidir. |

Ayrıntılar için [modlar ve izinler kılavuzuna](../MODES.md) bakın.

## Ne yapar

### 0.10.2'deki yenilikler

- `/pet on`, terminalinizi ajanın ne yaptığını gösteren hareketli bir balinaya
  dönüştürür; bu sırada mesaj kutusu kullanılabilir kalır. 980 parçacığı o anki
  adımı İngilizce, Çince veya Japonca olarak yazar.
- `/workbar terminal`, ajanın çalıştırdığı kabukları çalışırken gösterir. Birine
  yazın ya da kendinize ait bir kabuk için `Ctrl+N` tuşuna basın (macOS ve Linux).
- `/undo`, son isteğinizin tamamını, değiştirdiği her dosya dahil tek adımda geri
  alır; `/diff` de Git kullanılmayan klasörlerde çalışır.
- Bir plan hazır olduğunda Codewhale, Work'e Ask ile mi yoksa Auto-Review ile mi
  başlanacağını ya da planlamaya devam edilip edilmeyeceğini sorar.
- `/color`, her sohbete oturumla birlikte kaydedilen kendi yazma kutusu rengini
  verir.
- `web_search`, DeepSeek'in yeni Search API'sini kullanabilir: bir DeepSeek
  anahtarıyla `[search] provider = "deepseek"` ayarlayın.
- Full Access dışında, bir kimlik bilgisini yükleyecek bir `curl` veya `wget`
  onayınız olmadan asla çalışmaz; görünmez Unicode gizleyen bir komut ise
  reddedilir. `write` artık ajanın okumadığı ya da okuduktan sonra değişmiş bir
  dosyanın üzerine yazmaz.

### Modeller

Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter ve xAI dahil
40'tan fazla sağlayıcı yerleşik olarak gelir. OpenAI uyumlu her uç nokta çalışır;
Ollama, vLLM veya SGLang üzerinden yerel modeller de çalışır. Varsayılan model,
DeepSeek'in `deepseek-flash` modelidir; DeepSeek bu modeli 2026-10-17'de
DeepSeek-V4.1-Flash-1017'ye taşıdı. 0.10.1 onu güncelleme gerektirmeden çalıştırır
([ayrıntılar](../PROVIDERS.md#deepseek-v41-flash)).

`/model` turlar arasında model değiştirir; `/model auto` ise her tur için bir model
seçer (seçimin nasıl yapılacağını `/router` ayarlar). `/turn inspect`, bir turun
hangi modeli kullandığını, ne kadara mal olduğunu ve ne kadar sürdüğünü gösterir.
`codewhale auth chatgpt`, API anahtarı yerine bir ChatGPT planı kullanmanızı sağlar
(deneysel). Bkz. [Sağlayıcılar](../PROVIDERS.md).

### Makbuzlar ve geri alma

`/receipts` (veya çıktıktan sonra `codewhale receipts --last`), değiştirilen
dosyaları, çalıştırılan komutları, web ve MCP çağrılarını, başlatılan ajanları,
onayları ve bunları kimin verdiğini ve hataları listeler. `/undo` son isteğinizi ve değiştirdiği dosyaları tek adımda, güven modu olmadan geri alır. `/restore` tüm çalışma alanını daha önceki bir anlık görüntüye geri
döndürür; bu, sonraki düzenlemelerin üzerine yazdığı için `/trust on` veya Full
Access gerektirir. Bkz. [Makbuzlar](../RECEIPTS.md).

Araçların kullanılabilir olduğu exec çalıştırmaları her çıktı biçiminde
kaydedilir; böylece `codewhale receipts --last`, `--json` dahil olmak üzere
`exec --auto` sonrasında çalışır. Yalnızca yanıt üreten metin ve `--json`
çalıştırmaları ancak devam ettirilirken veya konuşma sıkıştırması başladığında
kaydedilir; makbuzlar veya `exec --continue` için bunları her zaman kaydetmek
üzere `--output-format stream-json` kullanın.

### Daha uzun işler

`/goal`, turlar boyunca geçerli kalan bir hedef belirler. Codewhale işi
[alt ajanlara](../SUBAGENTS.md) bölebilir, bir [ajan filosu](../FLEET.md)
çalıştırabilir veya depoya eklenmiş bir [iş akışını](../WORKFLOW_AUTHORING.md)
izleyebilir.

### Başka bir ajandan geçiş

Codewhale `AGENTS.md` dosyasını kendiliğinden okur. `CLAUDE.md`, `.cursorrules`
veya `.github/copilot-instructions.md` gibi başka araçlar için yazılmış talimat
dosyaları, yalnızca yapılandırmanızda `project_instruction_imports = ["claude"]`
(veya `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) ile bunu açıkça
seçtiğinizde yüklenir; bkz. [config.example.toml](../../config.example.toml). Codewhale,
yüklemediği böyle bir dosya bulduğunda uyarır.

`/import-claude`, Claude Code ayarlarınızın ve MCP sunucularınızın içe aktarımını
gözden geçirmeniz için hazırlar. `/plugin install`, beceriler, komutlar, ajan
profilleri ve MCP sunucuları içeren Claude Code eklentilerini kabul eder; kanca
veya LSP sunucusu gerektiren eklentileri reddeder
([ayrıntılar](../CLAUDE_PLUGIN_COMPAT.md)).

### Uzantılar

[MCP sunucuları](../MCP.md) bağlayın, [beceriler](../SKILLS.md) ve
[eklentiler](../PLUGINS.md) ekleyin, oturum ve araç olaylarında
[kancalar](../HOOKS.md) çalıştırın. Code mode (deneysel, varsayılan olarak açık),
modelin birkaç MCP ve eklenti aracını çağıran tek bir `execute_tools` programı
yazmasını sağlar; her çağrı yine onay kurallarınızdan geçer ve makbuzunu korur.
Ayrı olarak yüklenen Computer Use eklentisi
(deneysel, yalnızca macOS) diğer uygulamaları gözlemleyip kullanabilir. Siz
erişimini gözden geçirip etkinleştirene kadar kapalı kalır
([kılavuz](https://github.com/codewhale-hq/codewhale-cu-plugin)).

## Çalıştırma yolları

| Komut | Ne yapar |
| --- | --- |
| `codewhale` | Etkileşimli terminal uygulaması |
| `codewhale exec "…"` | Arayüz olmadan tek bir görev. Tek başına araç kullanmadan yanıt verir; `--auto` varsayılan olarak araçları Full Access ile etkinleştirir, `--sandbox workspace-write` bunları kısıtlar, `--output-format stream-json` olayları JSON olarak akış halinde verir |
| `codewhale web` | `127.0.0.1` üzerinde yerel bir web arayüzü ([kılavuz](../WEB.md)) |
| `codewhale review --pr N` | Bir GitHub pull request'inin tavsiye niteliğinde incelemesi. [GitHub bot kurulumu](../GITHUB_ACTION.md#set-up-your-own-bot), otomatik incelemeler ve `@codewhale review` için sürümü sabitlenmiş bir workflow üretir; kendi model anahtarınızı ve isteğe bağlı App kimliğini kullanır |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md); iş parçacıkları, olaylar, onaylar ve her turun dosya değişiklikleri için yerel bir HTTP API |
| `codewhale serve --acp` | Zed gibi editörler için stdio üzerinden Agent Client Protocol |

Terminal uygulaması, `exec`, `web` ve Runtime API tek bir motoru paylaşır
([mimari](../ARCHITECTURE.md)). Topluluk tarafından sürdürülen bir
[VS Code eklentisi](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode)
kenar çubuğundan bu motora bağlanır ([kaynak](https://github.com/HengQuWorld/CodeWhale-VSCode)).
Runtime API üzerine kurulu, platforma özgü bir masaüstü uygulaması geliştiriliyor;
henüz herkese açık bir indirmesi yok.

## Maliyet ve hesaplar

Codewhale ücretsizdir ve MIT lisanslıdır. Kendi API anahtarınızla ödemeyi doğrudan
sağlayıcıya yaparsınız; yerel modeller anahtar gerektirmez. Alt bilgi satırı ve
`/cost`, fiyat biliniyorsa bir oturumun maliyetini gösterir; bilinmeyen bir fiyat
asla ücretsiz olarak değil, bilinmeyen olarak gösterilir.

Hesaba ihtiyacınız yok. `/provider` ile eklediğiniz anahtarlar makinenizde kalır.

## Güvenlik ve gizlilik

Ask ve Auto-Review izinlerinde komutlar, sistemde varsa bir işletim sistemi
korumalı alanında çalışır: macOS'ta Seatbelt, Linux'ta ise kuruluysa bubblewrap.
Windows'ta henüz işletim sistemi korumalı alanı yoktur ve Full Access korumalı
alanı kapatır. Korumalı alanın içinde kabuk komutları `~/.ssh`, `~/.aws` ve `.env`
dosyaları gibi kimlik bilgisi depolarını okuyamaz; 0.10.2'den itibaren aynı kural
terminal oturumlarını ve daha fazla dosya aracını da kapsar. `/preview-request`, bir sonraki isteğin gizlenmiş (redacted) bir
önizlemesini, isteği göndermeden gösterir.

Telemetri varsayılan olarak açıktır. Her 90 günde bir değişen rastgele bir kurulum
kimliğiyle birlikte toplu sayımlar (sürüm ve platform, oturum süresi ve sonucu,
kullandığınız yerleşik sağlayıcılar, özellik ve hata sayıları) gönderir;
konuşmalar, kod, istemler, model adları, dosya veya depo adları ya da kimlik
bilgileri asla gönderilmez.
`codewhale config set telemetry false` telemetriyi kapatır. Codewhale başlangıçta
ayrıca yeni bir sürüm için GitHub'ı kontrol eder; bunu atlamak için
`CODEWHALE_NO_UPDATE_CHECK=1` ayarlayın. Daha fazlası için
[yetkilendirme sırası](../AUTHORIZATION_ORDER.md),
[korumalı alan](../SANDBOX.md) ve [telemetri](../TELEMETRY.md) belgelerine
bakın.

## Belgeler

| Buradan başlayın | Başvuru |
| --- | --- |
| [Kurulum](../INSTALL.md) | [Yapılandırma](../CONFIGURATION.md) |
| [Sağlayıcılar ve yerel modeller](../PROVIDERS.md) | [Mimari](../ARCHITECTURE.md) |
| [Modlar ve izinler](../MODES.md) | [Runtime API](../RUNTIME_API.md) |
| [Tuş atamaları](../KEYBINDINGS.md) | [Eklenti geliştirme](../PLUGIN_AUTHORING.md) |
| [GitHub PR incelemesi](../GITHUB_ACTION.md) | [Tüm belgeler](../README.md) |

## Topluluk

Bir sağlayıcı eksikse veya bir iş akışı zahmetliyse
[bir issue açın](https://github.com/codewhale-hq/CodeWhale/issues/new/choose) ya da
[pull request gönderin](../../CONTRIBUTING.md). Katkıda bulunanlar, kabul edilen işin
hakkını korur; [depo yapısı](../../CONTRIBUTING.md#project-structure) neyin nerede
olduğunu gösterir.

[Discord](https://discord.gg/37gfS3ksug) üzerinde sohbet edin veya projenin
bakımcısı Hunter'ı WeChat'te (`hunterbown`) ekleyip Whale Brothers grubuna katılmak
istediğinizi söyleyin.

## Geçmiş ve lisans

Codewhale, `deepseek-tui` olarak başladı ve hâlâ o projenin yapılandırmasını ve
oturumlarını okur. Bağımsız olarak sürdürülür ve DeepSeek ya da başka herhangi bir
model sağlayıcısıyla bağlantılı değildir.
[Katkıda bulunan herkese](../CONTRIBUTORS.md) teşekkürler.

[![Codewhale contributors](../assets/contributors.png)](../CONTRIBUTORS.md#everyone)

[MIT](../../LICENSE). Diğer açık kaynak projelerden uyarlanan kod
[üçüncü taraf bildirimlerinde](../THIRD_PARTY_NOTICES.md) listelenir.
