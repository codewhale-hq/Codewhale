<!-- source: README.md sha256:fd76d718e3a2 -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../brand/mark-reversed.svg">
  <img src="../../brand/mark-mono.svg" alt="Codewhale" width="72" height="72">
</picture>

<h1>Model herkesin olabilir. Ajan sizin olmalı.</h1>

Codewhale, terminaliniz için Rust ile yazılmış açık kaynaklı bir kodlama ajanıdır. Her modelle çalışır, bir görevi birkaç ajana bölebilir ve her satırını okuyabilirsiniz.

```bash
curl -fsSL https://codewhale.net/install.sh | sh
```

Bu komut, macOS veya Linux'a en son sürümü kurar ve hesap gerektirmez. [Windows, npm ve Cargo](#kurulum) aşağıda.

[![Latest release](https://img.shields.io/github/v/release/codewhale-hq/Codewhale?label=release)](https://github.com/codewhale-hq/Codewhale/releases/latest)
[![CI](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml/badge.svg)](https://github.com/codewhale-hq/Codewhale/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../../LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/37gfS3ksug)

[Web sitesi](https://codewhale.net) · [Belgeler](../README.md) · [Değişiklik günlüğü](../../CHANGELOG.md) · [Katkıda bulunma](../../CONTRIBUTING.md)

[English](../../README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja-JP.md) · [Tiếng Việt](README.vi.md) · [Bahasa Indonesia](README.id.md) · [한국어](README.ko-KR.md) · [Español](README.es-419.md) · [Português](README.pt-BR.md) · [Русский](README.ru.md) · [Українська](README.uk.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [繁體中文](README.zh-TW.md) · [हिन्दी](README.hi.md) · [Italiano](README.it.md) · [Polski](README.pl.md) · [العربية](README.ar.md) · [Català](README.ca.md)

<sub>Bu çeviri makine taslağıdır ve henüz anadili konuşan biri tarafından gözden geçirilmedi.</sub>

<img src="../../web/public/codewhale-tui-0102-demo.png" width="760" alt="Bir demo görevinden sonra terminalde Codewhale. export.py dosyasına --json bayrağı eklediğini, 3 yerel testin de geçtiğini ve devredilen incelemenin sorun bulmadığını bildiriyor. Alttaki görev listesinde tamamlanmış üç adım var ve durum satırında Full Access ile yerel website-demo sağlayıcısı görünüyor.">

<sub>Betiklenmiş bir test modeliyle, henüz yayınlanmamış 0.10.3'ün geliştirme derlemesi, tek kullanımlık bir çalışma alanında Full Access ile. Düzenleme, üç test ve bir alt ajan incelemesi gerçekten çalıştı.</sub>

</div>

## Kurulum

Yukarıdaki komut, sağlama toplamıyla doğrulanmış `codewhale` ile kısa takma adı `codew`'yi `~/.local/bin` içine koyar. Kabuğunuz ardından `command not found` derse, kurucunun yazdırdığı PATH notunu izleyin ([ayrıntılar](../INSTALL.md#put-it-on-your-path)). `codewhale update` onu yükseltir.

```bash
winget install --id HunterBown.CodeWhale --exact --source winget  # Windows x64
npm install -g codewhale                                          # wraps the same release binaries
cargo install codewhale-cli --locked                              # builds from source; can trail GitHub Releases
```

Hazır ikili dosyalar x64 ve arm64 üzerinde Linux, macOS ve Windows'u kapsar. Scoop, Nix, Linux'ta Homebrew, Android ve Termux (önizleme) ile elle indirme [kurulum kılavuzunda](../INSTALL.md), konteynerler ise [Docker](../DOCKER.md) sayfasında. Her makinede tek bir kurulum yöntemi kullanın; aksi hâlde `PATH` üzerinde iki kopya olur ve önce bulunan kazanır.

## Hızlı başlangıç

1. Çalışmanızı commit edin veya stash'leyin, sonra bir git deposunun kökünde `codewhale` çalıştırın. Varsayılan olarak depodaki dosya düzenlemelerini sormadan uygular ve yalnızca okuma yapmayan her kabuk komutundan önce sorar.
2. `/provider` çalıştırın (veya `F3`'e basın), bir sağlayıcı seçin, API anahtarını yapıştırın ve bir model seçin. Varsayılan sağlayıcı DeepSeek'tir. Anahtar olmadan ilk açılışta Codewhale, ilk mesajınızı gönderdiğinizde bir sağlayıcı sorar ve yerel bir Ollama sunucusu yanıt verirse yerel bir model önerir.
3. Bir görev yazın:

```text
Fix the failing tests and explain what changed.
```

Salt okunur komutlar hemen çalışır. Diğer her kabuk komutu için Codewhale önce sorar: `y` bir kez çalıştırır, `a` sohbetin geri kalanında benzer komutlara izin verir, `n` reddeder. `/undo` son isteğinizi ve dosya değişikliklerini geri alır, `/help` komutları ve tuşları listeler.

Betikler ve CI için `codewhale exec` arayüz olmadan tek bir görev çalıştırır. `--auto` ile ona araç vermeden önce [arayüzsüz çalıştırma](../INSTALL.md#headless-scripts-ci) bölümünü okuyun, çünkü bir sandbox eklemedikçe `--auto` Full Access kullanır.

## Ne yapar

Codewhale'e bir görev verin; görev bitene ya da girdinize ihtiyaç duyana kadar depoyu okur, dosyaları düzenler ve komutlar çalıştırır. [Son sürümün notları](https://github.com/codewhale-hq/Codewhale/releases/latest) en yeni değişiklikleri listeler, [değişiklik günlüğü](../../CHANGELOG.md) ise her sürümü içerir.

### Her model

Anthropic, OpenAI, DeepSeek, Google, Mistral, Moonshot, OpenRouter ve xAI dahil 40'tan fazla sağlayıcı yerleşik gelir. OpenAI uyumlu her uç nokta çalışır; Ollama, vLLM veya SGLang üzerinden yerel modeller de çalışır. Varsayılan model DeepSeek'in `deepseek-flash` modelidir ve artık DeepSeek V4.1 Flash tarafından sunulur ([ayrıntılar](../PROVIDERS.md#deepseek-v41-flash)).

`/model` turlar arasında model değiştirir, `/model auto` her tur için bir model seçer (nasıl seçileceğini `/router` ayarlar). `/turn inspect` bir turun hangi modeli kullandığını, maliyetini ve ne kadar sürdüğünü gösterir. Bkz. [Sağlayıcılar](../PROVIDERS.md).

### Makbuzlar ve geri alma

`/receipts` veya çıktıktan sonra `codewhale receipts --last`, bir oturumun değiştirdiği dosyaları, çalıştırdığı komutları, web ve MCP çağrılarını, başlattığı ajanları, her onayı ve onayı kimin verdiğini ve neyin başarısız olduğunu listeler. `/undo` son isteğinizi ve değiştirdiği dosyaları tek adımda geri alır. `/restore` tüm çalışma alanını daha önceki bir anlık görüntüye döndürür; bu sonraki düzenlemelerin üzerine yazdığı için `/trust on` veya Full Access gerektirir. Bkz. [Makbuzlar](../RECEIPTS.md).

### Bir görevde birkaç ajan

`/goal`, turlar boyunca süren bir hedef belirler. Codewhale işi [alt ajanlara](../SUBAGENTS.md) bölebilir, kendi rolleri ve modelleri olan ajanlardan oluşan bir [Fleet](../FLEET.md) çalıştırabilir veya depoya kaydedilmiş bir [iş akışını](../WORKFLOW_AUTHORING.md) izleyebilir.

### Uzantılar

[MCP sunucularını](../MCP.md) bağlayın, [beceriler](../SKILLS.md) ve [eklentiler](../PLUGINS.md) ekleyin, oturum ve araç olaylarında [kancalar](../HOOKS.md) çalıştırın. Kod modu (deneysel, varsayılan olarak açık), modelin birkaç MCP ve eklenti aracını çağıran tek bir `execute_tools` programı yazmasına izin verir; her çağrı yine onay kurallarınıza uyar ve makbuzunu korur. v0.10.2 ile gelen Computer Use eklentisi (deneysel, yalnızca macOS) diğer uygulamaları izleyip kullanabilir ve siz erişimini inceleyip etkinleştirene kadar kapalı kalır ([kılavuz](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/crates/tui/plugins/computer-use/README.md)). 0.10.3 ile gelecek: eklenti [ayrı olarak](https://github.com/codewhale-hq/codewhale-cu-plugin) yayımlanır.

### Çalıştırma yolları

| Komut | Ne yapar |
| --- | --- |
| `codewhale` | Etkileşimli terminal uygulaması |
| `codewhale exec "…"` | Arayüz olmadan tek bir görev, betikler ve CI için ([arayüzsüz çalıştırma](../INSTALL.md#headless-scripts-ci)) |
| `codewhale web` | `127.0.0.1` üzerinde yerel bir web istemcisi ([kılavuz](../WEB.md)) |
| `codewhale review --pr N` | Bir GitHub pull request'i için tavsiye niteliğinde bir inceleme; [GitHub botu kurulumu](../GITHUB_ACTION.md#set-up-your-own-bot) onu kendi model anahtarınızla çalıştırır |
| `codewhale app-server --http` | [Runtime API](../RUNTIME_API.md); iş parçacıkları, olaylar, onaylar ve her turun dosya değişiklikleri için yerel bir HTTP API |
| `codewhale serve --acp` | Zed gibi düzenleyiciler için stdio üzerinden Agent Client Protocol |

Terminal uygulaması, `exec`, `web` ve Runtime API tek bir motoru paylaşır ([mimari](../ARCHITECTURE.md)). Topluluk tarafından sürdürülen bir [VS Code uzantısı](https://marketplace.visualstudio.com/items?itemName=HengQuWorld.brotherwhale-vscode) kenar çubuğundan ona bağlanır ([kaynak](https://github.com/HengQuWorld/CodeWhale-VSCode)).

## Nasıl güvende kalır

İki anahtar, Codewhale'in sormadan ne kadar şey yapabileceğini belirler. Giriş boşken `Tab` modu değiştirir: **Plan** okur ve arar ama hiçbir şeyi değiştirmez, **Work** dosyaları düzenler ve komutlar çalıştırır, **Operate** ise Work ile aynı erişime sahiptir ve hedef doğrulanana ya da size ihtiyaç duyana kadar devam eder. `Shift+Tab` izni değiştirir. Varsayılan olan **Ask**, deponuzdaki dosya düzenlemelerini sormadan uygular ve yalnızca okuma yapmayan kabuk komutlarından önce sorar. **Auto-Review**, kuralların ve bir inceleme modelinin rutin çağrıları onaylamasına izin verir ve yüksek riskli olanları reddeder; bunları kendiniz onaylamak için Ask'e geçin. **Full Access** hiç sormaz ve Codewhale'in sandbox'ını kapatır, ancak depo kuralları ve yönetilen politika geçerli olmaya devam eder. Ayrıntılar [v0.10.2 kılavuzunda](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/MODES.md).

0.10.3 ile gelecek: `Shift+Tab`, Ask, Accept Edits, Plan, Auto-Review ve Full Access arasında dönen tek bir kadran olur, `Tab` artık modu değiştirmez ve Operate kaldırılır. Ask her dosya düzenlemesinden önce de soracak, yeni varsayılan Accept Edits ise git deposu içindeki düzenlemeleri uygulayıp komutlardan önce sormaya devam edecek. [Güncel kılavuz](../MODES.md) bunu anlatır.

Ask ve Auto-Review'de komutlar, sistemde varsa bir işletim sistemi sandbox'ında çalışır: macOS'ta Seatbelt, Linux'ta kuruluysa bubblewrap. Windows'ta henüz işletim sistemi sandbox'ı yok ve Full Access sandbox'ı kapatır. Sandbox içinde kabuk komutları ve terminal oturumları `~/.ssh`, `~/.aws` ve `.env` dosyaları gibi kimlik bilgisi depolarını okuyamaz. `/preview-request`, bir sonraki isteğin hassas bilgileri gizlenmiş önizlemesini göndermeden gösterir.

Hesaba ihtiyacınız yok ve `/provider` ile eklediğiniz anahtarlar makinenizde kalır. Kendi API anahtarınızla sağlayıcıya doğrudan ödeme yaparsınız; yerel modeller anahtar gerektirmez. Fiyat biliniyorsa alt bilgi ve `/cost` bir oturumun maliyetini gösterir; bilinmeyen fiyat ücretsiz olarak değil, bilinmiyor olarak gösterilir.

Telemetri varsayılan olarak açıktır. Toplu sayılar (sürüm ve platform, oturum süresi ve sonucu, kullandığınız yerleşik sağlayıcılar, özellik ve hata sayıları) ile her 90 günde bir değişen rastgele bir kurulum kimliği gönderir; sohbetleri, kodu, istemleri, model adlarını, dosya veya depo adlarını ya da kimlik bilgilerini asla göndermez. Bu komut onu kapatır:

```bash
codewhale config set telemetry false
```

Codewhale başlangıçta GitHub'da yeni bir sürüm olup olmadığını da denetler; atlamak için `CODEWHALE_NO_UPDATE_CHECK=1` ayarlayın. Daha fazlası [yetkilendirme sırası](../AUTHORIZATION_ORDER.md), [sandbox](../SANDBOX.md) ve [telemetri](../TELEMETRY.md) sayfalarında.

## Nereye gidiyor

Codewhale; terminalinizi, bir masaüstü uygulamasını, tarayıcıyı ve bulut bilgisayarlarını kapsayan tek bir ortam hâline geliyor; hepsi tek bir motorla çalışıyor. Aşağıdaki her bölüm, bugün nerede durduğunu söyler.

| Bölüm | Durum | Bugün |
| --- | --- | --- |
| Terminal uygulaması | Yayımlandı | macOS, Linux veya Windows'a kurun ve herhangi bir depoda `codewhale` çalıştırın. |
| Yerel tarayıcı istemcisi | Yayımlandı | Sohbeti okumak, görev göndermek ve onayları yanıtlamak için tarayıcınızda `codewhale web` çalıştırın; terminalle birlikte gelir ve hesap gerektirmez. |
| Bir görevde birçok ajan | Yayımlandı | Alt ajanlar, Fleet'ler ve iş akışları, birkaç ajanın her biri kendi rolüyle bir görevi paylaşmasını sağlar. |
| Masaüstü uygulaması | Geliştiriliyor, indirme yok | Aynı motor üzerinde bir masaüstü uygulaması geliştiriyoruz ve henüz herkese açık bir indirmesi yok. |
| Bulut bilgisayarları | Yalnızca davetle | Bulut bilgisayarları yalnızca davetle erişilebilir; bir oturum bulutta bir makinede çalışmaya devam edebilsin diye onları geliştiriyoruz. |
| Hepsini kapsayan tek bir oturum | Yapım aşamasında | Sizinle terminalden masaüstüne, tarayıcıya ve buluta giden ve yol boyunca sizin kalan tek bir oturum geliştiriyoruz. |

[Yol haritası](../ROADMAP.md), her sürümün neyi kapsadığını ve nerede yardım edebileceğinizi söyler.

## Başka bir ajandan geliyorsanız

Codewhale `AGENTS.md` dosyasını kendiliğinden okur. `CLAUDE.md`, `.cursorrules` veya `.github/copilot-instructions.md` gibi başka araçlar için yazılmış talimat dosyaları, yalnızca yapılandırmanızda `project_instruction_imports = ["claude"]` (veya `"cursor"`, `"cline"`, `"windsurf"`, `"copilot"`, `"all"`) ile etkinleştirdiğinizde yüklenir; bkz. [config.example.toml](../../config.example.toml). Codewhale, yüklemediği böyle bir dosya bulduğunda uyarır.

`/import-claude`, Claude Code ayarlarınızın ve MCP sunucularınızın içe aktarımını incelemeniz için hazırlar. `/plugin install`, beceriler, komutlar, ajan profilleri ve MCP sunucuları içeren Claude Code eklentilerini kabul eder; kanca veya LSP sunucusu gerektiren eklentileri reddeder ([ayrıntılar](../CLAUDE_PLUGIN_COMPAT.md)).

## Belgeler

Bu kılavuzlar ana dalı izler ve henüz son sürümde olmayan değişiklikleri anlatabilir.

| Buradan başlayın | Başvuru |
| --- | --- |
| [Kullanıcı kılavuzu](../GUIDE.md) | [Yapılandırma](../CONFIGURATION.md) |
| [Kurulum](../INSTALL.md) | [Mimari](../ARCHITECTURE.md) |
| [Sağlayıcılar ve yerel modeller](../PROVIDERS.md) | [Runtime API](../RUNTIME_API.md) |
| [Modlar ve izinler](../MODES.md) | [Eklenti yazma](../PLUGIN_AUTHORING.md) |
| [Tuş atamaları](../KEYBINDINGS.md) | [Makbuzlar](../RECEIPTS.md) |
| [GitHub pull request incelemesi](../GITHUB_ACTION.md) | [Tüm belgeler](../README.md) |

## Katkıda bulunma

Codewhale, onu kullanan insanlar tarafından birçok dilde açıkta geliştirilir.

<a href="../CONTRIBUTORS.md#everyone"><img src="../assets/contributors.png" width="760" alt="Codewhale katkıda bulunanlar duvarı: kodu, ortak yazarlı commit'leri, devam ettirilen pull request'leri veya adı anılan bildirimleri Codewhale'de bulunan her kişi için bir avatar ve en üstte toplam sayı."></a>

[Katkıda bulunanlar duvarı](../CONTRIBUTORS.md), kodu, ortak yazarlı commit'leri, devam ettirilen pull request'leri veya adı anılan bildirimleri Codewhale'de bulunan herkesi sayar; GitHub'ın kendi katkıda bulunanlar listesinin kaçırdığı kişiler de buna dahildir. `scripts/contributor-wall.py` resmi ve listeyi git geçmişinden ve GitHub'dan yeniden oluşturur.

Her geri bildirim memnuniyetle karşılanır: bir hata, bir fikir ya da yanlış görünen veya hissettiren bir şey. Bunların her biri için [bir issue açın](https://github.com/codewhale-hq/Codewhale/issues/new/choose); yeni bir özelliğe başlamak için de en iyi yer bir issue'dur. Düzeltmeler, çeviriler, sağlayıcılar ve planlanmış işler için pull request'ler memnuniyetle karşılanır. Sorular [Discussions](https://github.com/codewhale-hq/Codewhale/discussions) bölümüne. [Katkı kılavuzu](../../CONTRIBUTING.md), her yardım türünün nereye gittiğini, çalışmanızın adınızla nasıl birleştirildiğini ve [neyin nerede olduğunu](../../CONTRIBUTING.md#project-structure) gösterir.

[Discord](https://discord.gg/37gfS3ksug) üzerinden sohbet edin ya da proje sorumlusu Hunter'ı WeChat'te (`hunterbown`) ekleyip Whale Brothers grubuna katılmak istediğinizi söyleyin. Herkes hoş karşılanır.

## Tarihçe ve lisans

Codewhale `deepseek-tui` olarak başladı ve hâlâ o projenin yapılandırmasını ve oturumlarını okur. Bağımsız olarak sürdürülür ve DeepSeek'e ya da başka bir model sağlayıcısına bağlı değildir.

[MIT](../../LICENSE). Diğer açık kaynak projelerden uyarlanan kod, üçüncü taraf bildirimlerinde [dahil edilen kaynaklar](../../THIRD_PARTY_NOTICES.md) ve [uyarlanan tasarımlar](../THIRD_PARTY_NOTICES.md) için listelenir.
